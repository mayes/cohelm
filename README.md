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

Then point your MCP client at `http://127.0.0.1:47600/mcp`, for example `claude mcp add --transport http cohelm http://127.0.0.1:47600/mcp`. The first time a client calls a tool, Cohelm asks you in a prompt whether to allow it; Cohelm's toolbar button opens the Agents list, where you can disconnect one and copy these details (ADR 0007). Quitting the browser stops the Helper. `COHELM_PROFILE_DIR` (default: a Cohelm folder in your app data), `COHELM_HELPER_ADDR`, `COHELM_EXTENSION_DIR` (the built-in extension, `../extension` in this repo) and `COHELM_DEVTOOLS_MCP` (a local Chrome DevTools MCP script) change the defaults. An installed Cohelm finds all of these inside its own app and logs to `Helper.log` beside the profile (ADR 0006).

Cohelm looks for a newer release on GitHub shortly after it starts and twice a day after that. When there is one, the toolbar button shows a badge and the Agents list links to the release notes and the download. It never downloads or installs anything itself. `COHELM_UPDATE_FEED` points the check at another feed, and setting it empty turns the check off.
