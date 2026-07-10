## Overview

This gateway is created to (1) keep real API keys completely away from the client, and (2) provide fine-grained budget control functionality.

Real upstream API keys (DeepSeek, OpenAI, Anthropic) live only in the Cloudflare worker's secrets — they do not need to be set at the client. Instead, you create an opaque token scoped to:

1. a single **provider** — the token only works against the provider it was created for
2. an **expiration time**
3. an **IP allowlist** — checked against `CF-Connecting-IP`, which Cloudflare's edge sets from the real TCP connection and a client cannot spoof
4. a **spending budget** (in USD)

Point your client's base URL at the gateway and use the opaque token in place of a real API key. On each request, the gateway looks up the token from D1 database, verifies it's unexpired, from an allowed IP, and under budget, then substitutes in the real upstream secret and forwards the request.

This implementation is designed to be hosted on Cloudflare (it uses Workers and D1 database).

## Limitations

**Budget control is NOT strict.** We only check if the opaque token's current cost has exceeded its budget at the beginning of a request. As a result, one large request or concurrent requests can go over budget.
- One possible enhancement: at the beginning of a request, get the opaque token's current cost, and set `max_tokens` for the request. This is not implemented because I want to avoid modifying requests (other than injecting the real API key).

**Budget tracking is NOT 100% reliable.** If 1. client/upstream fails abruptly, or 2. the gateway fails to calculate the cost of the request or persist it to database: the request is still served and simply goes unbilled.
- One possible enhancement: implement proper monitoring/alerting. When this happens frequently (especially #2), there might be a bug.

## Supported use cases

| Provider  | Endpoint                       | Authorization method    | Agentic coding tool | Last verified                                   |
|-----------|--------------------------------|-------------------------|---------------------|-------------------------------------------------|
| DeepSeek  | /deepseek/chat/completions     | API key                 | OpenCode            | [@er91](https://github.com/er91) 2026-07-14    |
| DeepSeek  | /deepseek/v1/messages          | API key                 | Claude Code CLI     | [@er91](https://github.com/er91) 2026-07-14    |
| OpenAI    | /openai/v1/responses           | API key                 | OpenCode            | [@er91](https://github.com/er91) 2026-07-14    |
| OpenAI    | /openai/v1/responses           | API key                 | Codex CLI           | [@er91](https://github.com/er91) 2026-07-14    |
| Anthropic | /anthropic/v1/messages         | API key or OAuth token  | Claude Code CLI     | [@er91](https://github.com/er91) 2026-07-14    |

For now, only static credentials are supported — a plain API key, or a long-lived access token like the one `claude setup-token` generates. Claude Code CLI and Codex CLI also support a refresh-token + access-token mode, where the client periodically exchanges a refresh token for a new short-lived access token. Supporting that would require either:

1. implementing token refresh in the gateway itself — but the refresh flow is not a public or stable API, or
2. deriving a stable user identifier or session identifier from the ever-changing access tokens — which isn't possible, at least for Anthropic's tokens

## Build and deploy

In the Cloudflare dashboard:
- Create a new Worker.
- Create a new D1 database and run the SQL in `db_init.sql` against it (via the dashboard's D1 console or `wrangler d1 execute`).

Copy `wrangler.toml.template` to `wrangler.toml`, and set `YOUR_WORKER_NAME`/`YOUR_D1_DB_ID`/`YOUR_D1_DB_NAME` accordingly.

Install Cloudflare Wrangler.

Install Rust with `rustup` — the build needs to install the `wasm32-unknown-unknown` target.

Run `wrangler deploy` to deploy. Then add the upstream provider secrets you need (see the provider-specific sections below) via the dashboard's worker settings or `wrangler secret put <NAME>` (e.g. `wrangler secret put DEEPSEEK_API_KEY`).

## Create a test token

Callers authenticate with an opaque budget token stored in the `tokens` D1 table — never with a real upstream API key. Each token is scoped to exactly one provider, and enforces a budget, an expiration, and an optional IP allowlist.

Get `EXPIRATION_TIME` by running `new Date('YYYY-MM-DD HH:mm:SS').getTime()` in JavaScript, then insert a row:

```sql
insert into tokens values('test-token-deepseek', 'deepseek', 1.0, 0.0, 0.0, EXPIRATION_TIME, '["YOUR_IP"]')
```

Columns, in order:
- `token` — the opaque string clients authenticate with.
- `provider` — which upstream provider this token is valid for (currently `deepseek`, `openai`, or `anthropic`); a token only works against the provider it was created for.
- `provisioned_cost` — budget in USD.
- `current_cost` — spent so far; start at `0.0`.
- `creation_time` — creation time of this token, stored as milliseconds since Unix epoch (matching `Date.now()`). Not used for now; `0.0` is fine.
- `expiration_time` — expiration time of this token, stored as milliseconds since Unix epoch (matching `Date.now()`). Use `EXPIRATION_TIME` from above.
- `allowed_ips` — a JSON array of allowed client IPs, or `["*"]` to allow any IP.

Repeat this insert with a different `token` value for each client/provider pair you want to authorize.

## Use the DeepSeek/OpenAI provider for OpenCode

Add your DeepSeek API key as `DEEPSEEK_API_KEY` and/or your OpenAI API key as `OPENAI_API_KEY` to the worker's "Variables and secrets", then add the following to `~/.config/opencode/opencode.json` (replace `YOUR_WORKER_DOMAIN` with your actual worker domain):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "deepseek": {
      "options": {
        "baseURL": "https://YOUR_WORKER_DOMAIN/deepseek"
      }
    },
    "openai": {
      "options": {
        "baseURL": "https://YOUR_WORKER_DOMAIN/openai/v1"
      }
    }
  }
}
```

Create test tokens as described above with `provider` set to `'deepseek'` and `'openai'`, then set them in `~/.local/share/opencode/auth.json`:

```json
{
  "deepseek": {
    "type": "api",
    "key": "test-token-deepseek"
  },
  "openai": {
    "type": "api",
    "key": "test-token-openai"
  }
}
```

Restart OpenCode so these settings take effect.

## Use the OpenAI provider for Codex CLI

Add your OpenAI API key as `OPENAI_API_KEY` to the worker's "Variables and secrets", then create a test token as described above with `provider` set to `'openai'` (e.g. token `'test-token-openai'`).

Add the following to your `~/.codex/config.toml` (replace `YOUR_WORKER_DOMAIN` with your actual worker domain):

```toml
model_provider = "my_custom_endpoint"

[model_providers.my_custom_endpoint]
name = "My Custom Endpoint"
base_url = "https://YOUR_WORKER_DOMAIN/openai/v1"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
```

Then run Codex:

```
export OPENAI_API_KEY="test-token-openai"
codex
```

## Use DeepSeek provider for Claude Code CLI

DeepSeek also exposes an Anthropic-compatible `/v1/messages` endpoint, so you can point Claude Code at DeepSeek instead of Anthropic. Make sure your DeepSeek API key is added as `DEEPSEEK_API_KEY` to the worker's "Variables and secrets". We can reuse the test token created in the "Create a test token" section above (`provider` set to `'deepseek'`).

Unlike the Anthropic provider below, DeepSeek has no separate OAuth flow — both `Authorization: Bearer <token>` and `X-Api-Key: <key>` are accepted here, and both get mapped to the same `DEEPSEEK_API_KEY` secret. Set only one (requests with both headers set are rejected):

```
export ANTHROPIC_BASE_URL="https://YOUR_WORKER_DOMAIN/deepseek"  # replace YOUR_WORKER_DOMAIN with your actual worker domain
export ANTHROPIC_AUTH_TOKEN="test-token-deepseek"  # or: export ANTHROPIC_API_KEY="test-token-deepseek"
claude
```

## Use the Anthropic provider for Claude Code CLI

The gateway accepts either credential a client would normally send to Anthropic directly, and injects the matching real secret before forwarding upstream:
- An OAuth token, sent as `Authorization: Bearer <token>` → the gateway injects the `ANTHROPIC_OAUTH_TOKEN` secret.
- An API key, sent as `X-Api-Key: <key>` → the gateway injects the `ANTHROPIC_API_KEY` secret.

Send only one — requests with both headers set are rejected. You only need to configure whichever secret you actually plan to use.

Create a test token as described above with `provider` set to `'anthropic'` (e.g. token `'test-token-anthropic'`).

### OAuth token

Run `claude setup-token` to get a long-lived OAuth token, and add it as `ANTHROPIC_OAUTH_TOKEN` to the worker's "Variables and secrets".

Run Claude Code CLI:
```
export ANTHROPIC_BASE_URL="https://YOUR_WORKER_DOMAIN/anthropic"  # replace YOUR_WORKER_DOMAIN with your actual worker domain
export ANTHROPIC_AUTH_TOKEN="test-token-anthropic"
# Do not set ANTHROPIC_API_KEY
claude
```

### API key

Add your Anthropic API key as `ANTHROPIC_API_KEY` to the worker's "Variables and secrets".

Run Claude Code CLI:
```
export ANTHROPIC_BASE_URL="https://YOUR_WORKER_DOMAIN/anthropic"  # replace YOUR_WORKER_DOMAIN with your actual worker domain
export ANTHROPIC_API_KEY="test-token-anthropic"
# Do not set ANTHROPIC_AUTH_TOKEN
claude
```
