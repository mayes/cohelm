// Cohelm's built-in extension. The Helper loads it into every Cohelm window.
// For now it only exists, so the packaged browser can show that it loaded;
// Pause prompts, the connected-Agents list and Background windows come in
// their own tickets.
//
// A service worker runs only for an event, so it listens for each start of the
// browser as well as for being installed; the Helper looks for it running.
const ready = () => console.info("Cohelm extension ready");
chrome.runtime.onInstalled.addListener(ready);
chrome.runtime.onStartup.addListener(ready);
