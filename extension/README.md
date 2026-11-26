# Cohelm built-in extension

The small extension the Helper loads into Cohelm with `--load-extension`. It will hold the Pause prompts, the connected-Agents list, the Background window list and the taken-over-tab indicator. For now it's a skeleton that shows it loaded.

Its ID is fixed by the public key in `manifest.json`, so the Helper and the tests can find its pages:

`dojelhkgldmdbffaafmofoilmbbacppo`

Only the public key is kept. The extension is always loaded unpacked, so no private key is needed. Changing the key changes the ID.

Chrome-branded builds dropped `--load-extension` in Chrome 137. Chromium, ungoogled-chromium and Chrome for Testing keep it, which is what Cohelm and its tests run on.
