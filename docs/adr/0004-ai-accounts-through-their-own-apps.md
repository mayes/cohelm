# Claude and ChatGPT accounts connect through their own apps in 0.1

People want to use their Claude and ChatGPT accounts with Cohelm, not only API keys. Anthropic's consumer terms (February 2026) reserve Free, Pro and Max sign-ins for Anthropic's own apps, so Cohelm cannot offer "sign in with Claude". For 0.1 both accounts connect the same way: the Claude app, Claude Code or the ChatGPT app connects to the **Helper** over MCP and drives Cohelm as an **Agent**, on the person's own plan. The **Side panel** itself uses an API key (Anthropic or any OpenAI-compatible service, such as OpenRouter) or a local model (such as Ollama or LM Studio), with no provider favoured.

## Considered Options

- **Sign in with ChatGPT inside the Side panel**: OpenAI reportedly opened this to open-source apps in late 2026. Deferred past 0.1 to keep the first release simple; it needs Cohelm registered with OpenAI.
- **Asking Anthropic for approval**: possible under its Agent SDK terms, but unlikely and slow, and the app route covers the need.

## Consequences

0.1 also ships without telemetry or crash reporting, and tells people when a new release is out on GitHub without installing it; Cohelm follows each ungoogled-chromium security release within a week.
