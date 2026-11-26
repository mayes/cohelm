# Cohelm's pages talk to the Helper over the private pipe, and Agents are known by name

The person allows or declines each new **Agent** in a prompt from Cohelm's built-in extension, and manages them from its Agents list. Those pages need to reach the **Helper**, and whatever reaches the Helper this way can answer for the person.

The Helper reaches the pages over its private DevTools pipe (ADR 0005). It watches for pages of the built-in extension, attaches to each one for itself, gives it a `cohelmSend` function with `Runtime.addBinding`, and pushes it the lists by calling its `cohelmReceive`. The bridge keeps these sessions apart from Chrome DevTools MCP's: their events go only to the Helper, and Chrome DevTools MCP never hears of them. The Helper opens the prompt itself, in its own window, and only that window can answer for the Agent it asks about.

An MCP client is known by the name it gives itself. The first time a client the person hasn't allowed calls a tool, the call waits for the person's answer, up to a minute; listing tools doesn't wait. Allowed names are kept in `Cohelm Agents.json` in the profile, so they survive a restart and go with the profile. A declined name is refused until Cohelm restarts, so a client that retries doesn't ask again and again, and the person can allow it from the Agents list. Closing the prompt without answering declines. Disconnecting an Agent ends its sessions at once and forgets its approval, so it asks again next time.

## Considered options

- **The pages fetch from the Helper's HTTP port.** Any program on the computer can send the same requests, with any Origin, and so approve itself. The pipe can only be reached by the Helper.
- **A secret per client, in the address the person pastes.** It would tell clients apart for certain, but every MCP client would need its own address, and a program on the computer could read the secret from the client's settings anyway.
- **Messages through the extension's service worker.** It needs the same link to the Helper, and the worker can be stopped between events. Each page talking to the Helper directly is simpler.

## Consequences

- A client's name is its own claim. Approval keeps out clients the person didn't set up. It can't keep out a program on this computer that poses as an allowed one, but such a program could read the browser profile, and so the person's sessions, without Cohelm.
- Agents can't open the browser's own pages or Cohelm's (`chrome:`, `chrome-extension:` and the like). Chrome DevTools MCP doesn't list extension pages either, so an Agent can't press Allow for itself.
- The connection is served on loopback only, and the Helper refuses any connection that doesn't come from this computer.
- Clients on the stateless MCP protocol keep no session. They count as connected for a few minutes after their last call, and disconnecting one refuses its next call.
