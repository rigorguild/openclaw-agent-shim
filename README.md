# openclaw-agent-shim

Minimal HTTP-to-CLI shim that dispatches [openclaw](https://openclaw.ai) agents from webhooks. Workaround for the broken `hooks.mappings` dispatcher.

## Why this exists

openclaw 2026.4.x has bugs in webhook-driven agent dispatch via `hooks.mappings`:

- [#64556](https://github.com/openclaw/openclaw/issues/64556) — `hooks.mappings[].agentId` and `sessionKey` silently ignored for `action="wake"`
- [#70894](https://github.com/openclaw/openclaw/issues/70894) — webhook agent runs always start a new session regardless of `sessionKey` / session config

Net effect: the agent runs in the wrong session with the wrong prompt. This shim sidesteps it by accepting webhooks directly and shelling out to `openclaw agent --agent <id> -m <body> --json`. Pure pass-through, no template rendering, no prompt logic — all semantics live in the agent's workspace files.

The day openclaw fixes those issues, this shim is obsolete and the repo will be archived.

## How it works

```
n8n / GitHub / Notion webhook
   │
   ▼
POST /<agent_id>          (auth: Bearer <token>)
   │
   ▼
agent-shim
   │ spawns
   ▼
openclaw agent --agent <id> --session-id <unique> -m <body> --json
```

Per-run state is written to `RUNS_DIR/<runId>.json` and can be polled via `GET /runs/<runId>`. A unique `--session-id` is generated per webhook so each invocation gets a fresh claude-cli session — prevents history accumulation and language drift across calls.

## Install

Download a prebuilt binary from the [latest release](https://github.com/rigorguild/openclaw-agent-shim/releases/latest):

- `agent-shim-vX.Y.Z-linux-x86_64.tar.gz` — most servers
- `agent-shim-vX.Y.Z-linux-aarch64.tar.gz` — Raspberry Pi 4/5, ARM servers

```bash
curl -L https://github.com/rigorguild/openclaw-agent-shim/releases/latest/download/agent-shim-v0.1.0-linux-x86_64.tar.gz | tar -xz
chmod +x agent-shim
```

Or build from source (Rust 1.70+):

```bash
cargo build --release
# binary at target/release/agent-shim
```

## Quick start

Configure with env vars and run:

```bash
export AGENT_SHIM_TOKEN=$(openssl rand -hex 32)
export AGENT_SHIM_OPENCLAW_BIN=$(which openclaw)
./agent-shim
# → agent-shim listening on http://127.0.0.1:18790 (runs dir: /var/tmp/agent-shim/runs)
```

Dispatch a run for an agent already registered in `~/.openclaw/openclaw.json` (replace `myagent` with one of your `agents.list[].id`):

```bash
RUN=$(curl -s -X POST http://127.0.0.1:18790/myagent \
  -H "Authorization: Bearer $AGENT_SHIM_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"hello":"from webhook"}' | jq -r .runId)

# Poll until completion
curl -s http://127.0.0.1:18790/runs/$RUN | jq .
# → while running: {"shimRunId":"...","status":"running"}
# → when done: full openclaw output (exitCode, stderr, parsed openclaw JSON)
```

The request body is forwarded **verbatim** to your agent as the user-turn message. The shim does not parse, validate, or template it — your agent's runbook (e.g. `HEARTBEAT.md` or equivalent in its workspace) decides how to interpret the contents. Agents typically check whether the body parses as JSON and branch on a known field (`event`, `type`, etc.).

## Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/<agent_id>` | Dispatch a run. Body is forwarded verbatim as the agent message. Returns `{"ok":true,"agentId":"...","runId":"..."}`. |
| `GET` | `/runs/<runId>` | Read run state. While running: `{"shimRunId":"...","status":"running"}`. After: full openclaw output. |

## Configuration

The shim reads everything from environment variables:

| Variable | Default | Notes |
|---|---|---|
| `AGENT_SHIM_TOKEN` | (empty — auth disabled, warning logged) | Bearer token clients must send. **Set this in production.** |
| `AGENT_SHIM_BIND` | `127.0.0.1:18790` | `host:port` to listen on. Use `0.0.0.0:<port>` only behind a trusted network boundary (Tailscale, VPN, reverse proxy). |
| `AGENT_SHIM_OPENCLAW_BIN` | `openclaw` | Path to the openclaw CLI. Default relies on `$PATH`. Set explicitly if openclaw is installed under a custom location (e.g. nvm). |
| `AGENT_SHIM_RUNS_DIR` | `/var/tmp/agent-shim/runs` | Where per-run JSON state files are written. Created on startup if missing. |
| `AGENT_SHIM_OPENCLAW_CONFIG` | `$HOME/.openclaw/openclaw.json` | openclaw config used to validate that an incoming `agent_id` is actually registered. |

## Run

Direct:

```bash
AGENT_SHIM_TOKEN=changeme \
AGENT_SHIM_BIND=127.0.0.1:18790 \
AGENT_SHIM_OPENCLAW_BIN=$(which openclaw) \
./agent-shim
```

systemd user unit (example — adjust paths):

```ini
# ~/.config/systemd/user/agent-shim.service
[Unit]
Description=agent-shim — webhook → openclaw agent invocation
After=network-online.target

[Service]
Type=simple
EnvironmentFile=%h/agent-shim/.env
ExecStart=%h/agent-shim/target/release/agent-shim
Restart=on-failure

[Install]
WantedBy=default.target
```

Then:

```bash
systemctl --user daemon-reload
systemctl --user enable --now agent-shim
```

## Security

- **Always set `AGENT_SHIM_TOKEN`.** Without it the shim accepts any POST.
- **Do not expose the port to the public internet.** Front it with Tailscale, a VPN, or bind to localhost and reverse-proxy from a host that already has auth.
- The token is checked with a fixed-time-irrelevant equality comparison; this is not designed to resist a dedicated attacker who can already reach your local network.

## Status

Workaround. Will be archived once the upstream openclaw bugs are resolved.

## License

MIT
