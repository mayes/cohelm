# Cohelm built-in extension

The small extension the Helper loads into Cohelm with `--load-extension`. It holds the prompt where the person allows a new Agent (`allow.html`) and the Agents list with the details to connect one (`agents.html`, also the toolbar popup). The Pause prompts, the Background window list and the taken-over-tab indicator come in their own tickets.

The pages talk to the Helper only over its private DevTools pipe: the Helper attaches to each page and gives it `cohelmSend`, and calls the page's `cohelmReceive` with the newest lists (`link.js`, ADR 0007).

Its ID is fixed by the public key in `manifest.json`, so the Helper and the tests can find its pages:

`dojelhkgldmdbffaafmofoilmbbacppo`

Only the public key is kept. The extension is always loaded unpacked, so no private key is needed. Changing the key changes the ID.

Chrome-branded builds dropped `--load-extension` in Chrome 137. Chromium, ungoogled-chromium and Chrome for Testing keep it, which is what Cohelm and its tests run on.
