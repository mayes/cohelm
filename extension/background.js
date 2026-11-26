// Cohelm's built-in extension. The Helper loads it into every Cohelm window.
// For now it only exists, so the packaged browser can show that it loaded;
// Pause prompts, the connected-Agents list and Background windows come in
// their own tickets.
chrome.runtime.onInstalled.addListener(() => {
  console.info("Cohelm extension ready");
});
