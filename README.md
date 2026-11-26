# Cohelm

Open-source agent browser where you and your AI agents share the same sessions and logins. Connect Claude, ChatGPT or any MCP client, and risky steps pause for your OK by default.

Status: planning. Nothing to install yet.

License: Apache 2.0 (see `LICENSE`).

## Helper (in progress)

The Helper is a Rust process that serves MCP to your AI apps on this computer only and attaches to the tabs in your running browser through [Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp) (Apache 2.0), with Google's usage statistics and CrUX lookups switched off.

To try it against a Chromium started with `--remote-debugging-port=9222`:

```
cd helper
cargo run
```

Then point your MCP client at `http://127.0.0.1:47600/mcp`. `COHELM_BROWSER_URL`, `COHELM_HELPER_ADDR` and `COHELM_DEVTOOLS_MCP` (a local Chrome DevTools MCP script) change the defaults.
