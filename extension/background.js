// Cohelm's built-in extension. The Helper loads it into every Cohelm window.
// Its pages (allow.html, agents.html) talk to the Helper over its private
// DevTools pipe (see link.js); this worker only says it is running. Pauses and
// Background windows come in their own tickets.
//
// A service worker runs only for an event, so it listens for each start of the
// browser as well as for being installed; the Helper looks for it running.
const ready = () => console.info("Cohelm extension ready");
chrome.runtime.onInstalled.addListener(ready);
chrome.runtime.onStartup.addListener(ready);
