# Cohelm

An open-source browser that a person and their AI agents use together, sharing the same sessions and logins.

## Language

**Base browser**:
The existing open-source browser Cohelm is built on (ungoogled-chromium), which Cohelm patches and packages rather than replaces.
_Avoid_: upstream (ambiguous with Chromium itself), engine

**Agent**:
An AI that acts in the browser on a person's behalf, either through the built-in chat panel or an outside MCP client.
_Avoid_: bot, assistant

**Risky step**:
An action that sends, pays, submits or deletes, and so pauses for the person's OK by default.
_Avoid_: dangerous action, sensitive action

**Helper**:
Cohelm's Rust process that starts the **Base browser** over a private DevTools pipe and connects **Agents** to it, holding the **Pause** and the injection guard.
_Avoid_: daemon, sidecar, backend

**Side panel**:
The chat panel built into the browser window, where a person talks to an **Agent** and answers a **Pause**.
_Avoid_: sidebar, chat window

**Pause**:
The hold Cohelm puts on a **Risky step** until the person allows or blocks it.
_Avoid_: approval prompt, confirmation

**Background window**:
A Cohelm window on the person's same sessions where an **Agent** works out of sight until the person brings it up.
_Avoid_: headless browser, hidden tab

**Setup wizard**:
The first-run steps where the person imports from Chrome, signs in to sites, picks on-screen or background as the default, connects their first **Agent** and learns about the **Pause**.
_Avoid_: onboarding, first-run flow
