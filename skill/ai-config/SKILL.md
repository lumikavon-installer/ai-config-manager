---
name: ai-config
description: Read, modify, validate and connectivity-test the local AI Config Manager OpenAI configuration without revealing its API key. Uses the running desktop app's Unix domain socket when available and falls back to ~/.config/ai.yml.
---

# AI configuration

Use the installed `ai-config-manager-cli` binary built by this project. It shares the desktop application's Rust core, YAML rules, encryption envelope and OS vault. Never read the YAML directly into model context: legacy files may contain a plaintext API key.

## Automatic routing

- With no `--config`, `read`, `check`, `history`, `test` and `set` first try the desktop app's local agent socket. The socket is a Unix domain socket (AF_UNIX, also on Windows), normally at `~/.config/ai-config-manager/agent.sock`, protected by a private token file. A long home path may use a deterministic per-user temporary directory instead; the CLI resolves it automatically.
- If the app is not running, the same commands operate directly on `~/.config/ai.yml` using the shared file lock and optimistic revision.
- `--config PATH` always bypasses the agent and operates on that file directly.
- A save that was already sent to a connected agent is never retried blindly against the file; do that only after checking the current state.

## Operations

- `ai-config-manager-cli status`: whether the local agent is reachable, its socket path, token presence and the default configuration path. No secret is printed.
- `ai-config-manager-cli read`: JSON metadata only, including `exists`, `configured`, `endpoint`, `base_url`, `model`, `key_present`, `encrypted` and optimistic `revision`. Add `--endpoint embedding|laya|cosyvoice` for a non-chat interface.
- `ai-config-manager-cli check`: validate the YAML and decrypt the key in memory; returns only `{ "valid": true }`. No network request.
- `ai-config-manager-cli test`: explicit `GET {base_url}/models` network check. Returns a `ProbeResult` with `ok`, `status`, `endpoint`, `latency_ms` and a machine-readable `code` (`ok`, `unauthorized`, `endpoint_unsupported`, `server_error`, `redirect`, `timeout`, `network`, `rejected`, `client`). The response body is never returned. Named endpoints without a credential omit the Authorization header.
- `ai-config-manager-cli history`: last ten masked change records.
- `ai-config-manager-cli set`: update the configuration. Options:
  - `--endpoint openai|embedding|laya|cosyvoice` (default `openai`)
  - `--base-url URL`
  - `--model MODEL`
  - `--api-key-stdin` (required to change the key; read from stdin only; optional for local named endpoints)
  - `--encrypt` (default) or `--plaintext` (explicit opt-in)
  - `--expected-revision REV` (optional; otherwise the current revision is read first)
  - `--config PATH` (direct file mode)

Examples:

```sh
ai-config-manager-cli status
ai-config-manager-cli read
ai-config-manager-cli read --endpoint embedding
ai-config-manager-cli set --base-url https://api.example.com/v1 --model example-model
printf '%s' "$API_KEY" | ai-config-manager-cli set --api-key-stdin --encrypt
ai-config-manager-cli set --endpoint laya --base-url http://127.0.0.1:8848/v1 --model laya-multilingual
ai-config-manager-cli test
ai-config-manager-cli test --endpoint cosyvoice
```

`set` without `--api-key-stdin` preserves the saved key. `set` defaults to encrypted storage; an existing plaintext key is migrated on the next successful encrypted save. Never pass a key as an argument, environment dump, URL, log line or chat message. Never print the token file, YAML plaintext, ciphertext, request Authorization header or vault entry.

Success is `{ "ok": true, "data": ... }` with exit code 0; a command failure is `{ "ok": false, "error": { "code": "...", "message": "..." } }` with exit code 1. A connectivity test that completed but failed is still a successful operation: top-level `ok` is true and `data.ok` is false. Check `data.code`.

## REST over the local socket

For non-Rust integrations, the same AF_UNIX socket accepts HTTP/1.1 with `Authorization: Bearer <agent.token>` (or `X-Agent-Token`). Query `GET /ai-config`, `GET /ai-config/endpoints/{kind}`, `GET /sync`, register with `POST /sync/items`, configure with `POST /sync/settings`, remove with `DELETE /sync/items/{id}`, and run the explicit encrypted upload with `POST /sync/run`. Named endpoints use `/ai-config/endpoints/{kind}` with `/check`, `/test`, or `/decrypted` suffixes. Responses are `{ "ok": true, "data": ... }` or `{ "ok": false, "error": ... }`. The token file is private; never print it, and never send the GitHub token or encryption password to a model conversation.

## Secret handling and integration

- Never ask the user to paste the API key into a conversation. If a key must be changed, pipe it from the user's secure local source into `--api-key-stdin`.
- Do not use `--plaintext` unless the user explicitly requires it and accepts the risk.
- If a key cannot decrypt or the vault is locked/unavailable, stop and surface the error code. Do not rewrite the file, create a replacement vault key, downgrade encryption or fall back to a different configuration. A new key can replace unrecoverable ciphertext only when the user supplies it.
- Do not run `test` without the user's intent: it sends the saved credential to the configured endpoint.
- For actual model use, integrate the `ai-config-core` Rust library inside the trusted local process and consume the in-memory secret through its scoped configuration API. Keep the key inside that process and send it only to the endpoint the user has authorized.

## Concurrency and recovery

Reads may create a private lock file; history queries can finalize an already-committed pending journal. A successful history query does not validate the YAML; use `check` separately. `CONFLICT` means another writer changed the file or the agent saved between your read and write: re-read and reapply the intended field changes. `AGENT_UNAVAILABLE` means the app was not reachable and the direct file path was used where safe. `KEY_MISSING` / `DECRYPT_FAILED` require the user's recovery action. Never delete `.pending.json` blindly. The desktop UI remains the place for a human to confirm plaintext storage.
