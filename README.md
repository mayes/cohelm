# Cohelm

Open-source agent browser where you and your AI agents share the same sessions and logins. Connect Claude, ChatGPT or any MCP client, and risky steps pause for your OK by default.

Status: planning. Nothing to install yet.

License: Apache 2.0 (see `LICENSE`).

## Helper (in progress)

The Helper is a Rust process that starts the browser with a private DevTools pipe (no debugging port, see ADR 0005), serves MCP to your AI apps on this computer only, and lets them work in your open tabs through [Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp) (Apache 2.0), with Google's usage statistics and CrUX lookups switched off.

To try it with a Chromium or Chrome build:

```
cd helper
COHELM_BROWSER=/path/to/chromium cargo run -- https://example.com
```

Then point your MCP client at `http://127.0.0.1:47600/mcp`. Quitting the browser stops the Helper. `COHELM_PROFILE_DIR` (default: a Cohelm folder in your app data), `COHELM_HELPER_ADDR` and `COHELM_DEVTOOLS_MCP` (a local Chrome DevTools MCP script) change the defaults.
