# Cohelm

Open-source agent browser where you and your AI agents share the same sessions and logins. Connect Claude, ChatGPT or any MCP client, and risky steps pause for your OK by default. Apache 2.0.

## Workflow

All coding work in this repo uses Matt Pocock's skills, in this order:

1. `/grill-with-docs`: interview to sharpen the plan, writing `GLOSSARY.md` and ADRs as decisions land.
2. `/to-spec`: turn the plan into a spec on the issue tracker.
3. `/to-tickets`: break the spec into tracer-bullet tickets with blocking edges.
4. `/implement`: build from the spec and tickets.
5. `/code-review`: review the changes against the repo's standards and the spec.
6. `/retro`: retrospective on the session.

Don't skip a step without saying so. Small fixes can start at `/implement`, but still get `/code-review`. For a hard bug, start with `/diagnosing-bugs`. Not sure which skill fits? Run `/ask-matt`.

This repo is public. Keep secrets, credentials and private details out of commits.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues on `mayes/cohelm`. See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
