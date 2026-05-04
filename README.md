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

## Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/<agent_id>` | Dispatch a run. Body is forwarded verbatim as the agent message. Returns `{"ok":true,"agentId":"...","runId":"..."}`. |
| `GET` | `/runs/<runId>` | Read run state. While running: `{"shimRunId":"...","status":"running"}`. After: full openclaw output. |

## Build

```bash
cargo build --release
```

Pre-built binaries for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` are attached to each CI build (see Actions tab).

## Configuration

The shim reads from environment:

| Variable | Default | Notes |
|---|---|---|
| `AGENT_SHIM_TOKEN` | (empty — auth disabled, warning logged) | Bearer token clients must send. **Set this in production.** |

Other paths are currently hardcoded at the top of `src/main.rs` (`OPENCLAW_BIN`, `RUNS_DIR`, `OPENCLAW_CONFIG`, `BIND`). They match a Raspberry Pi 5 setup with openclaw installed via nvm. PRs to parametrize via env vars welcome.

## Run

Direct:

```bash
AGENT_SHIM_TOKEN=changeme ./agent-shim
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
