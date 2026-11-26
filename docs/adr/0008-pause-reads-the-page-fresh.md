# The Pause reads the page fresh, and pauses what it can't read

Every **Agent** tool call passes the **Helper**'s classifier before it reaches Chrome DevTools MCP. Tools name elements only by uid, so for a step that touches an element (a click, a fill, a drag, or Space while something has focus) the Helper takes its own snapshot of that page first. It reads the snapshot as structured data, which Chrome DevTools MCP gives when started with `--experimentalStructuredContent`, so text on a page or in an Agent's own arguments can't forge an element. Chrome DevTools MCP finds the uid in that same snapshot when it runs the step, so the step acts on the element the person is asked about. A uid that is no longer on the page is turned away, and so is a `pageId` that isn't a whole number, so the prompt and the step always name the same tab.

A click counts as pressing the nearest control that holds the element, so a click on an unnamed icon inside a "Place order" button presses "Place order". Outside any control, the words in and just around the element count.

A control whose name says send, pay, submit or delete (a fixed list of English words and phrases) is a **Risky step**, and so is anything Cohelm can't read: pressing Enter under any of its names, typing or filling text that holds a new line, typing or pressing a key outside a text box (where a key can be a page's shortcut, such as `#` for delete), ticking a box such as "I agree", uploading a file, accepting a dialog, reloading a page (which sends a form again without asking), running a script (including `initScript` and `javascript:` or `data:` addresses), a click at a spot, or a tool it doesn't know.

A Risky step waits at a **Pause**: a prompt window from Cohelm's built-in extension that names the Agent, the step, the site and what the step carries: the whole script with the elements it's given, the typed text, the files to upload, the dialog's question. A step carrying more than the prompt shows (`MOST_DETAIL`) is turned away rather than shown cut short, and a script sent as a file path is turned away, since the person can't read it. Allowing lets the call through; blocking, closing the prompt, not answering within two minutes (`PAUSE_WAIT`), cancelling the call or disconnecting the Agent returns an error that says why. Each Agent has one step waiting at a time. While it waits, an Agent whose client asked for progress hears "Waiting for the person to allow this" every ten seconds.

## Considered options

- **Read the snapshots Agents were given.** Cheaper, and the first build did this. Review showed an Agent or a page could forge snapshot lines in text, reuse a uid after the page reworded the button, or point the prompt at another tab's site. A fresh, structured snapshot of the page the call names closes all three.
- **Watch the page and stop the click there.** Intercepting input in the page needs code in every page, sees only DOM events, and races the page's own handlers. Reading the call before it's made stops the step before anything reaches the page.
- **Let scripts through.** Agents use scripts to read pages, so pausing them is noisy. But a script can click, send or delete anything, so for 0.1 it waits.

## Consequences

- Each click, fill or drag costs one extra snapshot of the page.
- Words are English only for 0.1. A page in another language pauses only through the steps Cohelm can't read, such as Enter and scripts.
- A control the classifier misses still goes through, and so does a page that changes in the moment between the Helper's snapshot and the step. Opening a link, closing a tab and changing a select don't pause. The word list errs toward pausing, and the injection guard after 0.1 adds another check.
- Without the built-in extension, nothing can ask the person, so every Risky step is blocked.
- An editor that types into an element without a text box role, such as some rich editors, pauses on every keystroke. Going back or forward in a tab doesn't pause, as Chromium won't send a form again that way without asking.
- The dialog's question comes from the tab Chrome DevTools MCP has selected, so a dialog on another tab is allowed without its question shown.
- The prompt's buttons wait a second each time its window comes forward or shows a new step, so a double-click meant for one prompt can't answer the one under it.
