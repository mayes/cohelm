# First release is an unsigned 0.1 beta built on our own machines

The first release is a 0.1 public beta for Mac and Windows, posted on GitHub Releases without code signing. Windows and Mac builds run on self-hosted build machines, because a full Chromium build takes hours and GitHub's hosted runners are neither big enough nor available on this account. Signing (an Apple developer account and a Windows certificate) waits until before 1.0, so 0.1 users will see their operating system's unknown-developer warnings.

## Consequences

0.1 scope is kept to the core: outside MCP clients (such as the Claude and ChatGPT apps) attaching to the person's open tabs on their same sessions, **Background windows**, the **Setup wizard**, and the **Pause** on **Risky steps**. **Side panel** chat, per-site rules, outside threat-signal feeds and plugins come after 0.1.
