# Provider catalog-to-support ledger (draft)

Fetch date: 2026-09-17 (UTC). Research-only; no repo edits.

## Pinned sources

| Client | Repo | SHA | Registry path |
|---|---|---|---|
| OpenCode | anomalyco/opencode | `5a8335857b0ebec44ef6aa1d52b339cf25c329ca` | `packages/opencode/src/provider/`, `packages/core/src/plugin/provider/`, live `https://models.opencode.ai/api.json` (fetched 2026-09-17 to `/tmp/models.opencode.ai-api.json`) |
| omp | can1357/oh-my-pi | `116190d317ca319ae17ab624cb479c76a1ca4704` | `packages/ai/src/registry/` + `packages/catalog/src/compat/rules/auth/*.kdl` + `packages/catalog/src/models.json` + `packages/ai/src/usage/` |
| Hermes | NousResearch/hermes-agent | `f5d192611032025d2757b07ad838921872126182` | `website/docs/integrations/providers.md`, `hermes_cli/auth.py` PROVIDER_REGISTRY, `hermes_cli/models_catalog_static.py` CANONICAL_PROVIDERS, `plugins/model-providers/*` |
| Codex docs | learn.chatgpt.com | live 2026-09-17 | `/docs/config-file/config-reference` (saved `/tmp/codex-config-reference.md`) |
| Claude docs | code.claude.com | live 2026-09-17 | `/docs/en/model-config`, `/docs/en/llm-gateway*`, `/docs/en/env-vars`, `/docs/en/plugins` |

## Legend

Evidence classes (from jackin-provider-research.md): **D** documented vendor contract, **S** first-party source, **R** reference implementation (omp/CodexBar/OpenUsage behavior, not a vendor commitment), **U** unresolved.

Auth shorthands: `env(KEY)` = API key from env var / staged secret; `oauth-*` = interactive login flow; `auth.json` = client credential store entry; `sdk` = AWS/GCP SDK chain (no static key).

Jackin launch-route shorthands (proposed):
- `OC` = launch `opencode` TUI in container with a staged per-instance XDG slice: filtered `$XDG_DATA_HOME/opencode/auth.json` single-provider entry + `opencode.json` provider/model preset; scrub ambient `*_API_KEY`.
- `OMP` = launch `omp` TUI with staged `PI_CODING_AGENT_DIR` slice: filtered SQLite `agent.db` credential row(s) for the selected account only + model preset; `OMP_PROFILE` isolation.
- `HER` = launch `hermes --tui -p <profile>` with staged `HERMES_HOME` profile slice: `auth.json` credential-pool entry + `.env` key + `config.yaml model:` block; no root-auth inheritance.

Jackin usage-source shorthands (proposed): `U(x)` = implement jackin-usage collector against endpoint x; `none:` + exact reason otherwise.

## 1. OpenCode

Live catalog 2026-09-17: **220 providers**. Pinned-SHA test fixture (`packages/opencode/test/tool/fixtures/models-api.json`): **159 providers**.
Delta: 62 added since fixture, 1 removed (`github-models`). Live-only: above, agentrouter, agnes, ai-router, ai21, aiand, aixy, aki-io, amd, arcee, blueclaw, bothub, cline-pass, coralbricks, crusoe, daoxe, ebcloud, echo, edenai, greenpt, hetzner, hyper, impossibl, inco, infer, infomaniak, iteracompute, jalapeno, klokintegration, kosmik, llmgateway-providers, llmtech, lynkr, melious, modal, modelis, nan, neosmith, oci, ofox, openreason, opper, pendra, qvac, runinfra, salad-cloud, scnet-token-plan, scx-ai, sensenova, standardcompute, stepfun-ai-step-plan, stepfun-step-plan, tensorx, thinkingmachines, tokengo, tokenrouter, vancine, vispark, volcengine, volcengine-coding-plan, wallaby, watsonx.

Auth architecture (S): credential = `auth.json` union {`oauth`{refresh,access,expires,accountId}, `api`{key,metadata}, `wellknown`{key,token}} at `$XDG_DATA_HOME/opencode/auth.json` (+`OPENCODE_AUTH_CONTENT` override); provider auth methods come from plugin `auth.provider` hooks (`oauth`|`api` + prompts); env keys per models.dev `env[]` also honored. Only 2 first-party OAuth integrations at this SHA: `openai` (ChatGPT browser PKCE localhost:1455 + headless device-code, client `app_EMoamEEZ73f0CkXaXp7hrann`) and `opencode` (device-code against opencode.ai/console, client `opencode-cli`).

Models config (S): each provider record = {api?, name, env[], id, npm?, models{modelId: {id,name,family?,release_date,attachment,reasoning,temperature,tool_call,reasoning_options?,cost?,limit{context,input?,output},modalities?,status?}}}; user overrides via config `provider.<id>` {apiKey,baseURL,models{},disabled} + variant options; snapshot baked at build (`OPENCODE_MODELS_DEV`), refreshed from `https://models.opencode.ai` (5-min TTL, hourly re-fetch; `OPENCODE_MODELS_URL`/`OPENCODE_MODELS_PATH` override).

Usage/observability (S): per-session token + cost accounting from models.dev `cost` (session/llm/ai-sdk.ts, acp/usage.ts); `includeUsage=true` forced on openai-compatible. **No per-provider quota/balance/credit API anywhere in the pinned tree** (zero usage/balance/credit refs in provider plugins). So every row below is usage-`none` except where an external first-party/reference endpoint is proposed.

| client | provider id | auth | jackin launch route (proposed) | jackin usage source (proposed or exact reason if none) | evidence class |
|---|---|---|---|---|---|
| opencode | 302ai (117m) | env(302AI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | abacus (108m) | env(ABACUS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | abliteration-ai (3m) | env(ABLIT_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | above (8m) | env(ABOVE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | agentrouter (5m) | env(AGENTROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | agnes (3m) | env(AGNES_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ai-router (5m) | env(AI_ROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ai21 (2m) | env(AI21_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | aiand (11m) | env(AIAND_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | aihubmix (78m) | env(AIHUBMIX_API_KEY); @aihubmix/ai-sdk-provider | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | aixy (1m) | env(AIXY_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | aki-io (7m) | env(AKI_IO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | alibaba (56m fp-plugin) | env(DASHSCOPE_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | alibaba-cn (89m) | env(DASHSCOPE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | alibaba-coding-plan (12m) | env(ALIBABA_CODING_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | alibaba-coding-plan-cn (12m) | env(ALIBABA_CODING_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | alibaba-token-plan (27m) | env(ALIBABA_TOKEN_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | alibaba-token-plan-cn (27m) | env(ALIBABA_TOKEN_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | amazon-bedrock (165m fp-plugin+loader) | AWS SDK chain (bearer/IAM/profile/ECS/IRSA); Converse/Mantle select | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ambient (10m) | env(AMBIENT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | amd (6m) | env(AMD_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | anthropic (14m fp-plugin+loader) | env(ANTHROPIC_API_KEY); catalog header transform | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | anyapi (30m) | env(ANYAPI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | arcee (7m) | env(ARCEE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | atomic-chat (5m) | env(ATOMIC_CHAT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | auriko (15m) | env(AURIKO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | azure (87m fp-plugin+loader) | env(AZURE_API_KEY)+AZURE_RESOURCE_NAME; chat/responses/messages select | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | azure-cognitive-services (74m fp-plugin+loader) | env key + AZURE_COGNITIVE_SERVICES_RESOURCE_NAME; deployment/compat URL build | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | bailing (2m) | env(BAILING_API_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | baseten (23m) | env(BASETEN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | berget (6m) | env(BERGET_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | blueclaw (2m) | env(BLUECLAW_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | bothub (8m) | env(BOTHUB_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cerebras (2m fp-plugin) | env(CEREBRAS_API_KEY); 3rd-party header | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | chutes (14m) | env(CHUTES_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | clarifai (12m) | env(CLARIFAI_PAT); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | claudinio (2m) | env(CLAUDINIO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cline-pass (15m) | env(CLINE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cloudferro-sherlock (5m) | env(CLOUDFERRO_SHERLOCK_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cloudflare-ai-gateway (44m fp-plugin+loader) | env(CLOUDFLARE_API_TOKEN/CF_AIG_TOKEN)+account/gateway id; native passthrough select | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cloudflare-workers-ai (27m fp-plugin+loader) | env(CLOUDFLARE_API_KEY); Workers AI binding | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cohere (14m fp-plugin) | env(COHERE_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | coralbricks (4m) | env(CORAL_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | cortecs (106m) | env(CORTECS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | crof (24m) | env(CROF_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | crossmodel (59m) | env(CROSSMODEL_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | crusoe (11m) | env(CRUSOE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | daoxe (9m) | env(DAOXE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | databricks (30m) | env(DATABRICKS_HOST,DATABRICKS_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | deepinfra (68m fp-plugin) | env(DEEPINFRA_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | deepseek (4m) | env(DEEPSEEK_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | digitalocean (97m) | env(DIGITALOCEAN_ACCESS_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | dinference (6m) | env(DINFERENCE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | drun (3m) | env(DRUN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ebcloud (4m) | env(EBCLOUD_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | echo (1m) | env(ECHO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | edenai (283m) | env(EDENAI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | empiriolabs (61m) | env(EMPIRIOLABS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | evroc (16m) | env(EVROC_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | fastrouter (47m) | env(FASTROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | fireworks-ai (33m) | env(FIREWORKS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | freemodel (10m) | env(FREEMODEL_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | friendli (7m) | env(FRIENDLI_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | frogbot (26m) | env(FROGBOT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | github-copilot (28m fp-plugin+loader) | Copilot OAuth token; chat-vs-responses route select (GPT-5+) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | gitlab (25m fp-plugin) | env(GITLAB_TOKEN)+GITLAB_INSTANCE_URL; Duo agentic/workflow chat | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | gmicloud (15m) | env(GMICLOUD_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | google (39m fp-plugin) | env(GOOGLE_GENERATIVE_AI_API_KEY/GOOGLE_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | google-vertex (52m fp-plugin+loader) | GCP ADC/service-account; regional endpoint build | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | google-vertex-anthropic (14m fp-plugin+loader) | GCP ADC; Anthropic-on-Vertex endpoint (us/eu REP domains) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | greenpt (40m) | env(GREENPT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | groq (16m fp-plugin) | env(GROQ_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | helicone (90m) | env(HELICONE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | hetzner (2m) | env(HETZNER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | hpc-ai (9m) | env(HPC_AI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | huggingface (77m) | env(HF_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | hyper (23m) | env(HYPER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | iflowcn (14m) | env(IFLOW_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | impossibl (76m) | env(IMPOSSIBL_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | inception (3m) | env(INCEPTION_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | inceptron (4m) | env(INCEPTRON_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | inco (7m) | env(INCO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | infer (2m) | env(INFER_API_KEY); @ai-sdk/openai | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | inference (9m) | env(INFERENCE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | inferx (12m) | env(INFERX_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | infomaniak (10m) | env(INFOMANIAK_API_KEY,INFOMANIAK_PRODUCT_ID); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | io-net (17m) | env(IOINTELLIGENCE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | iteracompute (9m) | env(ITERACOMPUTE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | jalapeno (17m) | env(JALAPENO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | jiekou (61m) | env(JIEKOU_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | kenari (59m) | env(KENARI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | kilo (377m fp-plugin) | env(KILO_API_KEY); gateway headers | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | kimi-for-coding (4m) | env(KIMI_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | klokintegration (3m) | env(KLOKINTEGRATION_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | kosmik (1m) | env(KOSMIK_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | kuae-cloud-coding-plan (1m) | env(KUAE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | lilac (4m) | env(LILAC_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | llama (7m) | env(LLAMA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | llmgateway (193m fp-plugin) | env key; gateway headers when integration present | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | llmgateway-providers (404m) | env(LLMGATEWAY_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | llmtech (1m) | env(LLMTECH_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | llmtr (32m) | env(LLMTR_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | lmstudio (3m) | env(LMSTUDIO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | longcat (1m) | env(LONGCAT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | lucidquery (4m) | env(LUCIDQUERY_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | lynkr (1m) | env(LYNKR_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | meganova (19m) | env(MEGANOVA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | melious (15m) | env(MELIOUS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | merge-gateway (187m) | env(MERGE_GATEWAY_API_KEY); merge-gateway-ai-sdk-provider | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | meta (5m fp-plugin+loader) | env(META_MODEL_API_KEY); Responses wire | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | minimax (7m) | env(MINIMAX_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | minimax-cn (7m) | env(MINIMAX_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | minimax-cn-coding-plan (7m) | env(MINIMAX_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | minimax-coding-plan (7m) | env(MINIMAX_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | mistral (34m fp-plugin) | env(MISTRAL_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | mixlayer (5m) | env(MIXLAYER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | moark (2m) | env(MOARK_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | modal (4m) | env(MODAL_PROXY_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | model-oracle-ai (15m) | env(MODEL_ORACLE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | modelis (9m) | env(MODELIS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | modelscope (7m) | env(MODELSCOPE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | moonshotai (4m) | env(MOONSHOT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | moonshotai-cn (4m) | env(MOONSHOT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | morph (3m) | env(MORPH_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | nan (7m) | env(NAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | nano-gpt (574m) | env(NANO_GPT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | nearai (32m) | env(NEARAI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | nebius (17m) | env(NEBIUS_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | neon (46m) | env(NEON_AI_GATEWAY_BASE_URL,NEON_AI_GATEWAY_TOKEN); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | neosmith (4m) | env(NEOSMITH_API_KEY); @ai-sdk/openai | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | neuralwatt (21m) | env(NEURALWATT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | nova (2m) | env(NOVA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | novita-ai (107m) | env(NOVITA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | nvidia (105m fp-plugin) | env(NVIDIA_API_KEY); billing-origin headers | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | oci (9m) | env(OCI_GENAI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ofox (143m) | env(OFOX_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ollama-cloud (24m) | env(OLLAMA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | openai (48m fp-plugin+loader) | oauth(ChatGPT browser/headless PKCE+refresh) or env(OPENAI_API_KEY); Responses wire | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | opencode (103m fp-plugin+loader) | oauth(device-code via opencode.ai/console) or env(OPENCODE_API_KEY); keyless free models w/ apiKey=public | OC | U(GET opencode.ai/zen/go/v1/usage, rolling/weekly/monthly; no Zen-balance/live-model fields) | S+R |
| opencode | opencode-go (37m) | env(OPENCODE_API_KEY); @ai-sdk/openai-compatible | OC | U(GET opencode.ai/zen/go/v1/usage, rolling/weekly/monthly; no Zen-balance/live-model fields) | S+R |
| opencode | openreason (3m) | env(OPENREASON_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | openrouter (369m fp-plugin) | env(OPENROUTER_API_KEY); referrer/title headers | OC | U(GET openrouter.ai/api/v1/auth/key, key-scoped; /credits needs mgmt key) | S+D |
| opencode | opper (40m) | env(OPPER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | orcarouter (117m) | env(ORCAROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | ovhcloud (15m) | env(OVHCLOUD_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | pendra (6m) | env(PENDRA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | perplexity (4m fp-plugin) | env(PERPLEXITY_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | perplexity-agent (22m) | env(PERPLEXITY_API_KEY); @ai-sdk/openai | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | pioneer (112m) | env(PIONEER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | poe (137m) | env(POE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | poolside (3m) | env(POOLSIDE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | privatemode-ai (11m) | env(PRIVATEMODE_API_KEY,PRIVATEMODE_ENDPOINT); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | qihang-ai (9m) | env(QIHANG_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | qiniu-ai (91m) | env(QINIU_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | qvac (9m) | env(QVAC_API_KEY); @qvac/ai-sdk-provider | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | regolo-ai (18m) | env(REGOLO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | requesty (153m) | env(REQUESTY_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | routing-run (15m) | env(ROUTING_RUN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | runinfra (7m) | env(RUNINFRA_GATEWAY_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | sakana (4m) | env(SAKANA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | salad-cloud (1m) | env(SALAD_CLOUD_API_KEY); @saladtechnologies-oss/ai-sdk-provider | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | sap-ai-core (49m fp-plugin+loader) | SAP AICore OAuth client-credentials (service binding) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | sarvam (2m) | env(SARVAM_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | scaleway (15m) | env(SCALEWAY_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | scnet-token-plan (18m) | env(SCNET_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | scx-ai (4m) | env(SCX_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | sensenova (5m) | env(SENSENOVA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | siliconflow (49m) | env(SILICONFLOW_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | siliconflow-cn (47m) | env(SILICONFLOW_CN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | snowflake-cortex (25m fp-plugin+loader) | programmatic token; Cortex REST | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | stackit (8m) | env(STACKIT_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | standardcompute (1m) | env(STANDARDCOMPUTE_API_KEY); @openrouter/ai-sdk-provider | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | stepfun (8m) | env(STEPFUN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | stepfun-ai (8m) | env(STEPFUN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | stepfun-ai-step-plan (3m) | env(STEPFUN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | stepfun-step-plan (4m) | env(STEPFUN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | subconscious (2m) | env(SUBCONSCIOUS_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | submodel (9m) | env(SUBMODEL_INSTAGEN_ACCESS_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | synthetic (10m) | env(SYNTHETIC_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tencent-coding-plan (8m) | env(TENCENT_CODING_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tencent-token-plan (2m) | env(TENCENT_TOKEN_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tencent-tokenhub (3m) | env(TENCENT_TOKENHUB_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tensorx (26m) | env(TENSORX_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | the-grid-ai (9m) | env(THEGRID_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | thinkingmachines (2m) | env(TINKER_API_KEY); @ai-sdk/anthropic | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tinfoil (8m) | env(TINFOIL_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | togetherai (39m fp-plugin) | env(TOGETHER_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tokengo (13m) | env(TOKENGO_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | tokenrouter (1m) | env(TOKENROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | trustedrouter (7m) | env(TRUSTEDROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | umans-ai (6m) | env(UMANS_AI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | umans-ai-coding-plan (7m) | env(UMANS_AI_CODING_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | unorouter (23m) | env(UNOROUTER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | upstage (4m) | env(UPSTAGE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | v0 (3m) | env(V0_API_KEY); @ai-sdk/vercel | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | vancine (8m) | env(VANCINE_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | venice (106m fp-plugin) | env(VENICE_API_KEY) | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | vercel (373m fp-plugin) | env key; referrer/title headers | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | vispark (3m) | env(VISPARK_LAB_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | vivgrid (27m) | env(VIVGRID_API_KEY); @ai-sdk/openai | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | volcengine (16m) | env(ARK_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | volcengine-coding-plan (10m) | env(ARK_CODING_PLAN_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | vultr (10m) | env(VULTR_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | wafer.ai (5m) | env(WAFER_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | wallaby (1m) | env(WALLABY_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | wandb (27m) | env(WANDB_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | watsonx (5m) | env(WATSONX_AI_APIKEY,WATSONX_AI_PROJECT_ID); watsonx-ai-provider | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | xai (12m fp-plugin+loader) | env(XAI_API_KEY); Responses wire | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | xiaomi (6m) | env(XIAOMI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | xiaomi-token-plan-ams (7m) | env(XIAOMI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | xiaomi-token-plan-cn (7m) | env(XIAOMI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | xiaomi-token-plan-sgp (7m) | env(XIAOMI_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | xpersona (13m) | env(XPERSONA_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zai (16m) | env(ZHIPU_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zai-coding-plan (7m) | env(ZHIPU_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zeldoc (1m) | env(ZELDOC_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zenifra (1m) | env(ZENIFRA_AI_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zenmux (120m fp-plugin) | env key; referrer/title headers | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zhipuai (15m) | env(ZHIPU_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |
| opencode | zhipuai-coding-plan (10m) | env(ZHIPU_API_KEY); @ai-sdk/openai-compatible | OC | none: no per-provider quota API in OpenCode; only local session tokens/cost | S |

## 2. omp (oh-my-pi)

Auth architecture (S): single registry `PROVIDER_REGISTRY` derived from `packages/catalog/src/compat/rules/auth/*.kdl` (84 providers + `_order.kdl` login roster); credentials in SQLite `agent.db` (`api_key`|`oauth`, multi-row account pools, refresh leases, `usage_history` table); login kinds: `oauth-code` (PKCE browser callback), `device-code`, `api-key` (paste+validate), `custom` (bespoke TS handler in `registry/oauth/`), none (env/config-only or bare rule). `auth-broker` (+gateway) mirrors credentials for remote use; refresh owned solely by AuthStorage.

Models config (S): `packages/catalog/src/models.json` (69 providers, bundled per-provider model lists w/ pricing/context/effort metadata) + `provider-models/` descriptors (special/openai-compat/ollama/google/cline-pass); user selects model per session; custom providers via `docs/adding-a-provider.md` (new auth KDL + catalog entry).

Usage/observability (S+R): 20 `UsageProvider`s in `DEFAULT_USAGE_PROVIDERS` (auth-storage.ts) with per-provider `supports` guards + ranking strategies; usage cache `usage_cache:*`; reference-grade private endpoints (R) unless vendor-documented (D).

| client | provider id | auth | jackin launch route (proposed) | jackin usage source (proposed or exact reason if none) | evidence class |
|---|---|---|---|---|---|
| omp | abliteration (api-key 3m) | api-key paste+validate (Abliteration) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | aiand (api-key 11m) | api-key paste+validate (ai&) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | aimlapi (no-login 349m) | env/config-only, no login flow (AIML API) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | alibaba-coding-plan (custom 12m) | custom TS handler (Alibaba Coding Plan) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | alibaba-token-plan (custom 8m) | custom TS handler (QwenCloud Token Plan) | OMP | U(bailian console BroadScopeAspnGateway token-plan API) | R |
| omp | amazon-bedrock (no-login 182m) | sdk(aws-bedrock hook: bearer/IAM/profile/ECS/IRSA); env hook="aws-bedrock" | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | anthropic (oauth-code 25m) | oauth-code PKCE (Anthropic (Claude Pro/Max)); refresh {; env hook="anthropic-foundry" | OMP | U(GET api.anthropic.com/api/oauth/usage + profile; OAuth scope-gated) | R |
| omp | azure (no-login 40m) | env/config-only, no login flow (Azure OpenAI) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S+U |
| omp | baseten (api-key 17m) | api-key paste+validate (Baseten) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | bedrock-mantle (no-login 5m) | sdk(aws-bedrock-mantle hook); env hook="aws-bedrock-mantle" | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | cerebras (api-key 8m) | api-key paste+validate (Cerebras) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | charm-hyper (api-key nomodels) | api-key paste+validate (Charm Hyper) | OMP | U(credits endpoint) | R |
| omp | cline-pass (api-key 20m) | api-key paste+validate (ClinePass) | OMP | U(/users/me + /users/me/plan/usage-limits) | R |
| omp | cloudflare-ai-gateway (custom 95m) | custom TS handler (Cloudflare AI Gateway) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | commandcode (api-key 69m) | api-key paste+validate (Command Code) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | coreweave (api-key 39m) | api-key paste+validate (CoreWeave Serverless Inference) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | cursor (custom 119m) | custom TS handler (Cursor (Claude, GPT, etc.)) | OMP | U(cursor.com/api/usage-summary + /api/auth/me; personal scope) | R |
| omp | deepinfra (api-key 106m) | api-key paste+validate (DeepInfra) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | deepseek (api-key 4m) | api-key paste+validate (DeepSeek) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | devin (oauth-code 2m) | oauth-code PKCE (Devin); refresh "token" | OMP | U(SeatManagementService/GetUserStatus; no REST usage endpoint) | R |
| omp | exa (api-key nomodels) | api-key paste+validate (Exa) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | firepass (api-key 2m) | api-key paste+validate (Fire Pass (Fireworks subscription)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | fireworks (api-key 35m) | api-key paste+validate (Fireworks) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | github-copilot (custom 47m) | custom TS handler (GitHub Copilot) | OMP | U(api.github.com copilot quota) | R |
| omp | gitlab-duo-agent (oauth-code 1m) | oauth-code PKCE (GitLab Duo Agent); refresh { | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | gitlab-duo (oauth-code 16m) | oauth-code PKCE (GitLab Duo Non-Agentic); refresh { | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | gmi-cloud (api-key 1m) | api-key paste+validate (GMI Cloud) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | google-antigravity (oauth-code 20m) | oauth-code PKCE (Antigravity (Gemini 3, Claude, GPT-OSS)); refresh { | OMP | U(daily-cloudcode-pa :retrieveUserQuotaSummary; bind identity) | R |
| omp | google-gemini-cli (oauth-code 7m) | oauth-code PKCE (Google Cloud Code Assist (Gemini CLI)); refresh { | OMP | U(cloudcode-pa Code Assist quota; OAuth only) | R |
| omp | google-vertex (no-login 31m) | sdk(google-vertex-adc hook); env hook="google-vertex-adc" | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | google (no-login 44m) | env/config-only, no login flow (Google Gemini) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S+U |
| omp | groq (no-login 20m) | env/config-only, no login flow (Groq) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S+U |
| omp | huggingface (api-key 76m) | api-key paste+validate (Hugging Face Inference) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | kagi (api-key nomodels) | api-key paste+validate (Kagi) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | kilo (custom 566m) | custom TS handler (Kilo Gateway) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | kimi-code (device-code 7m) | device-code (Kimi Code); refresh { | OMP | U(api.kimi.com/coding/v1/usages; OAuth + key paths) | R |
| omp | litellm (api-key nomodels) | api-key paste+validate (LiteLLM) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | llama.cpp (api-key nomodels) | api-key paste+validate (llama.cpp (Local OpenAI-compatible)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | lm-studio (api-key nomodels) | api-key paste+validate (LM Studio (Local OpenAI-compatible)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | meta (api-key 5m) | api-key paste+validate (Meta Model API) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | minimax-code-cn (api-key 9m) | api-key paste+validate (MiniMax Token Plan (China)) | OMP | U(same remains API on CN host; verify) | U |
| omp | minimax-code (api-key 9m) | api-key paste+validate (MiniMax Token Plan (International)) | OMP | U(api.minimax.io /v1/token_plan/remains) | S |
| omp | minimax (no-login 8m) | env/config-only, no login flow (MiniMax) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | mistral (no-login 31m) | env/config-only, no login flow (Mistral) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S+U |
| omp | moonshot (api-key 17m) | api-key paste+validate (Moonshot (Kimi API)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | muse-code (device-code 5m) | device-code (Muse Code (Subscription)); refresh "none" | OMP | U(cached MSP observation; key-exchange is separate op, never auto-poll) | R |
| omp | nanogpt (api-key 1074m) | api-key paste+validate (NanoGPT) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | novita (api-key 117m) | api-key paste+validate (Novita) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | nvidia (api-key 169m) | api-key paste+validate (NVIDIA) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | ollama-cloud (api-key 51m) | api-key paste+validate (Ollama Cloud) | OMP | none: no quota endpoint yet; register view only | S |
| omp | ollama (api-key nomodels) | api-key paste+validate (Ollama (Local OpenAI-compatible)) | OMP | none: no quota endpoint; register view only | S |
| omp | openai-codex-device (custom nomodels) | custom TS handler (ChatGPT Plus/Pro (Codex, headless/device)); refresh { | OMP | U(same as openai-codex; device grant lineage) | R |
| omp | openai-codex (oauth-code 6m) | oauth-code PKCE (ChatGPT Plus/Pro (Codex Subscription)); refresh { | OMP | U(wham/usage + reset-credit inventory; prefer app-server account/*) | R |
| omp | openai (no-login 55m) | env/config-only, no login flow (OpenAI) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S+U |
| omp | opencode-go (api-key 35m) | api-key paste+validate (OpenCode Go) | OMP | U(GET opencode.ai/zen/go/v1/usage) | R |
| omp | opencode-zen (api-key 95m) | api-key paste+validate (OpenCode Zen) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | openrouter (oauth-code 525m) | oauth-code PKCE (OpenRouter); refresh "none" | OMP | U(GET openrouter.ai/api/v1/auth/key, key-scoped) | D |
| omp | parallel (api-key nomodels) | api-key paste+validate (Parallel) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | perplexity (custom nomodels) | custom TS handler (Perplexity (Pro/Max)); refresh "none" | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | qianfan (api-key 1m) | api-key paste+validate (Qianfan) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | qwen-portal (api-key 2m) | api-key paste+validate (Qwen Portal) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | sakana (api-key 3m) | api-key paste+validate (Sakana AI) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | siliconflow-cn (api-key nomodels) | api-key paste+validate (SiliconFlow (China)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | siliconflow (api-key nomodels) | api-key paste+validate (SiliconFlow) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | synthetic (api-key 11m) | api-key paste+validate (Synthetic) | OMP | U(api.synthetic.new/v2/quotas) | R |
| omp | tavily (api-key nomodels) | api-key paste+validate (Tavily) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | together (api-key 39m) | api-key paste+validate (Together) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | typesafe (api-key nomodels) | api-key paste+validate (TypeSafe) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | umans (api-key 9m) | api-key paste+validate (Umans AI Coding Plan) | OMP | U(api.code.umans.ai /v1/usage) | R |
| omp | venice (api-key 156m) | api-key paste+validate (Venice) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | vercel-ai-gateway (api-key 308m) | api-key paste+validate (Vercel AI Gateway) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | vllm (api-key nomodels) | api-key paste+validate (vLLM (Local OpenAI-compatible)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | wafer-serverless (api-key 20m) | api-key paste+validate (Wafer Serverless (pay-as-you-go)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | xai-oauth (device-code 9m) | device-code (xAI Grok OAuth (SuperGrok or X Premium+)); refresh { | OMP | U(Grok CLI billing endpoint; weekly/monthly split) | R |
| omp | xai (api-key 31m) | api-key paste+validate (xAI API) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | xiaomi-token-plan-ams (api-key 4m) | api-key paste+validate (Xiaomi Token Plan (Europe)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | xiaomi-token-plan-cn (api-key 4m) | api-key paste+validate (Xiaomi Token Plan (China)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | xiaomi-token-plan-sgp (api-key 4m) | api-key paste+validate (Xiaomi Token Plan (Singapore)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | xiaomi (custom 6m) | custom TS handler (Xiaomi MiMo) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | yolo-auto (api-key 1m) | api-key paste+validate (Yolo-Auto) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | zai-coding-plan (oauth-code nomodels) | oauth-code PKCE (Z.AI (GLM Coding Plan · Sign in)); refresh "none" | OMP | U(same as zai; plan-scoped pools) | R |
| omp | zai (api-key 16m) | api-key paste+validate (Z.AI (GLM Coding Plan)) | OMP | U(api.z.ai /api/monitor/usage/quota/limit + model-usage; CREDIT vs TOKENS_LIMIT) | R |
| omp | zenmux (api-key 264m) | api-key paste+validate (ZenMux) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |
| omp | zhipu-coding-plan (api-key 15m) | api-key paste+validate (Zhipu Coding Plan (智谱)) | OMP | none: no omp UsageProvider and no vendor quota API evidenced | S |

Note: `minimax-cn` exists in models.json but has no auth KDL (gap in pinned source; auth unresolved = U). Tool/search-only providers (exa,kagi,tavily,parallel,typesafe) and local shells (ollama,lm-studio,llama.cpp,vllm,litellm) intentionally lack quota semantics.

## 3. Hermes

Auth architecture (S): `PROVIDER_REGISTRY` (auth.py, 38 rows: 6 oauth/bespoke + 32 api-key tuples) is credential truth; `CANONICAL_PROVIDERS` (models_catalog_static.py, 39 slugs) is the `hermes model` universe; `plugins/model-providers/*` (38 plugins) auto-extend both; `actual`+`alibaba-coding-plan`+`opencode-free` are registry-only; `router,commandcode(+commandcode-anthropic),deepinfra,meta-ai,nebius-token-factory,upstage,custom` are plugin-only; `alibaba-cn,-coding-plan-cn,-token-plan(-cn)` ride the alibaba plugins; `moa` is virtual (no credential/endpoint). Auth types: api_key, oauth_device_code (nous), oauth_external (openai-codex device-code + Codex-CLI import, xai-oauth, qwen-oauth), oauth_minimax, copilot (token/gh), external_process (copilot-acp stdio), aws_sdk (bedrock), vertex (ADC), keyless (opencode-free). Store: `~/.hermes/auth.json` (per-provider state + credential pool, fcntl-guarded) + `~/.hermes/.env` keys; profiles = separate Hermes homes under `~/.hermes/profiles/<name>` (config.yaml/.env/auth.json/state.db; no root-auth inheritance).

Models config (S): curated `_PROVIDER_MODELS` (42 keys) + live `/v1/models` probes + OpenRouter live catalog (disk-cached, TTL) + `$HERMES_HOME/models_dev_cache.json`; `config.yaml model:{default|model, provider|auto, base_url, api_mode}` + per-task auxiliary routing (`auxiliary.*.provider`, default auto→main model); `hermes model` = setup wizard, `/model` = in-session switch of configured providers.

Usage/observability (S): local-only session token accounting in state.db (input/output/cache_read/cache_write/reasoning + estimated/actual_cost_usd + billing_provider/route); `/usage` = session/context display; `/usage reset` redeems Codex reset credits (openai-codex only); Nous Portal billing/top-up/remote-spend via portal APIs. **No generic per-provider quota API**; per-provider billing scope documented in the subscription-plans table (plans differ; cells marked not-documented stay open questions).

| client | provider id | auth | jackin launch route (proposed) | jackin usage source (proposed or exact reason if none) | evidence class |
|---|---|---|---|---|---|
| hermes | nous | oauth_device_code (Portal; JWT inference:invoke, opaque fallback; client=hermes-client-v tag) | HER | U(Portal billing/balance APIs; verify scope) | S |
| hermes | nous-api | env(NOUS_API_KEY) aggregator path (config comment only) | HER | none: unresolved contract (U) | S |
| hermes | openai-codex | oauth_external device-code; imports ~/.codex/auth.json; dead-refresh quarantine | HER | U(Codex wham/usage + banked reset credits; prefer app-server) | S+R |
| hermes | openai-api | env(OPENAI_API_KEY)+opt OPENAI_BASE_URL | HER | none: Org usage/costs APIs need separate reporting authority; no key-scoped balance | S |
| hermes | xai-oauth | oauth_external browser login (SuperGrok/Premium+) | HER | U(Grok CLI billing endpoint; weekly/monthly split) | S+R |
| hermes | xai | env(XAI_API_KEY); codex_responses wire | HER | none: xAI API mgmt reporting separate; no key-scoped quota evidenced | S |
| hermes | qwen-oauth | oauth_external PKCE (reuses Qwen CLI login) | HER | none: no quota API evidenced | S |
| hermes | copilot | copilot token env(COPILOT_GITHUB_TOKEN,GH_TOKEN,GITHUB_TOKEN)/gh auth | HER | U(api.github.com copilot quota) | S+R |
| hermes | copilot-acp | external_process (spawns copilot --acp --stdio) | HER | none: external process owns quota; no Hermes-side API | S |
| hermes | gemini | env(GOOGLE_API_KEY,GEMINI_API_KEY); native Gemini client | HER | none: AI Studio has no key-scoped quota API; Code Assist path is separate OAuth | S |
| hermes | vertex | vertex OAuth2 (service-account/ADC); per-request regional base | HER | none: GCP billing/quotas via Cloud Billing, not in Hermes | S |
| hermes | zai | env(GLM_API_KEY,ZAI_API_KEY,Z_AI_API_KEY) (+pool) | HER | U(api.z.ai quota/limit + model-usage; plan/CN/team pools) | S+D |
| hermes | kimi-coding | env(KIMI_API_KEY,KIMI_CODING_API_KEY); sk-kimi- redirect to api.kimi.com/coding | HER | U(api.kimi.com/coding/v1/usages) | S+R |
| hermes | kimi-coding-cn | env(KIMI_CN_API_KEY) -> api.moonshot.cn | HER | U(same usages API on CN host; verify) | S |
| hermes | stepfun | env(STEPFUN_API_KEY) Step Plan | HER | none: no quota API evidenced | S |
| hermes | anthropic | env(ANTHROPIC_API_KEY,ANTHROPIC_TOKEN) | OAuth (Claude Max) | CLAUDE_CODE_OAUTH_TOKEN setup-token (prefix-routed) | HER | U(OAuth usage endpoint for OAuth grants; API keys need org reporting) | S+D |
| hermes | alibaba | env(DASHSCOPE_API_KEY) dashscope-intl compat | HER | none: no quota API evidenced | S |
| hermes | alibaba-cn | env(DASHSCOPE_API_KEY) mainland endpoint (plugin) | HER | none: no quota API evidenced | S |
| hermes | alibaba-coding-plan | env(ALIBABA_CODING_PLAN_API_KEY,DASHSCOPE_API_KEY) (registry-only) | HER | none: no quota API evidenced | S |
| hermes | alibaba-coding-plan-cn | env(ALIBABA_CODING_PLAN_CN_API_KEY,...) (plugin) | HER | none: no quota API evidenced | S |
| hermes | alibaba-token-plan | env(ALIBABA_TOKEN_PLAN_API_KEY) (plugin) | HER | U(bailian token-plan API; same family as omp collector) | S+R |
| hermes | alibaba-token-plan-cn | env(ALIBABA_TOKEN_PLAN_CN_API_KEY) (plugin) | HER | U(same on CN host; verify) | S |
| hermes | minimax | env(MINIMAX_API_KEY) -> /anthropic | HER | U(Token Plan remains + PAYG balance endpoints; scope-split) | S+D |
| hermes | minimax-cn | env(MINIMAX_CN_API_KEY) -> minimaxi /anthropic | HER | U(same on CN host; verify) | S |
| hermes | minimax-oauth | oauth_minimax browser PKCE (Coding Plan) | HER | U(same remains API w/ OAuth grant; verify) | S+D |
| hermes | deepseek | env(DEEPSEEK_API_KEY) | HER | none: no key-scoped quota API evidenced (balance endpoint unverified here) | S |
| hermes | nvidia | env(NVIDIA_API_KEY) NIM | HER | none: no quota API evidenced | S |
| hermes | ai-gateway | env(AI_GATEWAY_API_KEY) Vercel AI Gateway | HER | none: gateway-side metering only; no Hermes API | S |
| hermes | opencode-zen | env(OPENCODE_ZEN_API_KEY); x-opencode-session pinning | HER | none: no Zen balance API evidenced (do not invent) | S |
| hermes | opencode-go | env(OPENCODE_GO_API_KEY); mixed /v1 + /v1/messages by model | HER | U(GET opencode.ai/zen/go/v1/usage) | S+R |
| hermes | opencode-free | keyless anonymous (registry-only) | HER | none: no account exists to meter | S |
| hermes | kilocode | env(KILOCODE_API_KEY) | HER | none: no quota API evidenced | S |
| hermes | huggingface | env(HF_TOKEN) router | HER | none: no quota API evidenced | S |
| hermes | xiaomi | env(XIAOMI_API_KEY) | HER | none: no quota API evidenced | S |
| hermes | tencent-tokenhub | env(TOKENHUB_API_KEY) | HER | none: no quota API evidenced | S |
| hermes | tencent-tokenplan | env(TOKENPLAN_API_KEY) Anthropic-messages endpoint | HER | none: no quota API evidenced | S |
| hermes | ollama-cloud | env(OLLAMA_API_KEY) | HER | none: no quota endpoint evidenced | S |
| hermes | lmstudio | local http://127.0.0.1:1234/v1; opt LM_API_KEY | HER | none: local server, nothing to meter | S |
| hermes | custom | user base_url (+opt key); Ollama/vLLM/llama.cpp/ARK (plugin-only) | HER | none: arbitrary endpoint, no common quota API | S |
| hermes | bedrock | aws_sdk boto3 chain | HER | none: AWS metering via Cost Explorer/CloudWatch, not Hermes | S |
| hermes | azure-foundry | wizard endpoint+env(AZURE_FOUNDRY_API_KEY) | HER | none: Azure metering via Cost Management, not Hermes | S |
| hermes | arcee | env(ARCEEAI_API_KEY) | HER | none: no quota API evidenced | S |
| hermes | gmi | env(GMI_API_KEY) | HER | none: no quota API evidenced | S |
| hermes | actual | env(ACTUAL_API_KEY) hosted relay OR loopback keyless (registry-only) | HER | none: no quota API evidenced | S |
| hermes | router | env(RAMP_ROUTER_API_KEY,ROUTER_API_KEY); codex_responses gateway (plugin-only) | HER | none: gateway-side metering only | S |
| hermes | fireworks | env(FIREWORKS_API_KEY) (canonical+plugin) | HER | none: no key-scoped quota API evidenced | S |
| hermes | novita | env(NOVITA_API_KEY) (canonical+plugin) | HER | none: no quota API evidenced | S |
| hermes | nebius-token-factory | env(NEBIUS_API_KEY,...) (plugin-only) | HER | none: no quota API evidenced | S |
| hermes | commandcode | env(COMMANDCODE_API_KEY) chat_completions (plugin-only) | HER | none: no quota API evidenced | S |
| hermes | commandcode-anthropic | same COMMANDCODE_API_KEY; anthropic_messages (plugin-only) | HER | none: no quota API evidenced | S |
| hermes | deepinfra | env(DEEPINFRA_API_KEY) (plugin-only) | HER | none: no quota API evidenced | S |
| hermes | meta-ai | env(MODEL_API_KEY,META_API_KEY,...) Muse Spark; codex_responses (plugin-only) | HER | none: no quota API evidenced | S |
| hermes | upstage | env(UPSTAGE_API_KEY) Solar (plugin-only) | HER | none: no quota API evidenced | S |
| hermes | openrouter | env(OPENROUTER_API_KEY) or OAuth PKCE via `hermes auth add` (aggregator, not in auth registry) | HER | U(GET /api/v1/auth/key key-scoped; /credits needs mgmt key; exact model persisted) | S+D |
| hermes | moa | virtual (no credential/endpoint) | HER | none: aggregator preset, metered via reference models | S |

## 4. Codex `model_providers` wire_api (official docs, D)

Source: learn.chatgpt.com `/docs/config-file/config-reference` (fetched 2026-09-17). `model_providers.<id>`: custom provider table; built-ins `openai|ollama|lmstudio` reserved. Keys: `name, base_url, env_key (+env_key_instructions), experimental_bearer_token (discouraged), requires_openai_auth, wire_api, query_params, http_headers, env_http_headers, request_max_retries(4), stream_max_retries(5), stream_idle_timeout_ms(300000), supports_websockets, supports_standalone_web_search, auth{command,args,timeout_ms,refresh_interval_ms}` (command-backed Bearer [REDACTED] must not combine with env_key/bearer/requires_openai_auth).

**`wire_api`: type `responses`; "`responses` is the only supported value, and it is the default when omitted."** Chat-completions wire deprecated (openai/codex discussion #7782). Kimi/Z.AI officially support Codex Responses routes (per jackin-provider-research). Jackin implication: every Codex custom-provider launch stages a Responses-speaking `base_url` (+`env_key`); Chat/Anthropic-native backends need a protocol relay, never a `wire_api` flag.

## 5. Claude plugin / provider-routing options (official docs, D)

Plugins: `code.claude.com/docs/en/plugins` — plugins extend skills/agents/hooks/MCP only; **zero provider/model-routing surface** (verified by full-text search). No plugin-based provider routing exists officially.

Provider routing (all D, `env-vars` + `model-config` + `llm-gateway*`):
- Endpoint/credential: `ANTHROPIC_BASE_URL` (proxy/gateway; non-first-party host disables MCP tool search by default + Remote Control), `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` (Bearer), `apiKeyHelper` (+`CLAUDE_CODE_API_KEY_HELPER_TTL_MS`), cloud flags (`CLAUDE_CODE_USE_BEDROCK`, `ANTHROPIC_VERTEX_BASE_URL`, `ANTHROPIC_BEDROCK_BASE_URL`, Foundry/Agent Platform/Claude-on-AWS). Gateways must expose an Anthropic-format endpoint; routing to non-Claude models explicitly unsupported.
- Model select: `--model`/`/model`, `ANTHROPIC_MODEL`, `model` setting, `ANTHROPIC_DEFAULT_MODEL`, alias pins `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`, `CLAUDE_CODE_SUBAGENT_MODEL`, subagent frontmatter/`modelOverrides`, `availableModels`+`enforceAvailableModels`, org defaults; discovery: `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` populates `/model` from gateway `/v1/models`; window fix: `CLAUDE_CODE_MAX_CONTEXT_TOKENS`; embedders: `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST` (host owns routing; user/managed keys ignored).
- Jackin implication: Claude custom-provider launches = per-instance env (`ANTHROPIC_BASE_URL`+credential+`ANTHROPIC_MODEL`/alias pins) + isolated `CLAUDE_CONFIG_DIR`; never a plugin; `/status` verifies (base-URL + auth-token lines).

## 6. Jackin cross-client notes

- Same billing identity across clients must not multiply allowance: e.g. one Z.AI key in opencode+zai, omp+zai, hermes+zai, codex-profile, claude-wrapper = ONE account, FIVE launch routes, ONE quota source (api.z.ai pools). Ledger rows are launch routes, not accounts.
- OAuth refresh ownership: exactly one writer per grant lineage (opencode Auth, omp AuthStorage, hermes auth.json pool, or native CLI); jackin stages copies, never dual-refreshes.
- Unsupported-but-visible: every `none:` usage row stays on the Usage screen with its exact reason; `U` rows need Mac-live verification before claiming support.
