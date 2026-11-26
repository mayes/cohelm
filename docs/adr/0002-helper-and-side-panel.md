# Cohelm's own code lives in a Rust helper and a side panel, not in Chromium patches

Cohelm's own features run outside the Chromium source tree where they can: a Rust **Helper** process beside the browser serves the MCP connection, the **Pause** for the person's OK and the prompt-injection guard, and a built-in **Side panel** holds the chat. Agent control starts from Google's Chrome DevTools MCP (Apache 2.0), adapted to drive the person's own open window instead of launching a fresh one. Chromium is patched only where nothing else works, so each Chromium update is a small rebase instead of a merge of our features through the browser's code.

## Consequences

Some features (such as a native-feeling Side panel or blocking a step before the page sees it) may still need small Chromium patches; each one should be justified against this decision.
