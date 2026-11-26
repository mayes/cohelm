// The link between Cohelm's pages and the Helper. The Helper attaches to
// each Cohelm page over its private DevTools pipe, gives it cohelmSend to
// answer through, and calls cohelmReceive with the newest lists. Nothing
// else on the computer can reach the page this way.

/** Call render(state) with each state the Helper sends; returns send(message). */
export function connect(render, { onMissing } = {}) {
  globalThis.cohelmReceive = render;
  const send = (message) => {
    if (typeof globalThis.cohelmSend === "function") {
      globalThis.cohelmSend(JSON.stringify(message));
    }
  };
  const started = Date.now();
  let warned = false;
  const hello = setInterval(() => {
    if (typeof globalThis.cohelmSend === "function") {
      clearInterval(hello);
      send({ hello: true });
    } else if (!warned && Date.now() - started > 3000) {
      warned = true;
      onMissing?.();
    }
  }, 50);
  return send;
}

/** Copy text, and say so on the button for a moment. */
export async function copy(text, button) {
  try {
    await navigator.clipboard.writeText(text);
    const label = button.textContent;
    button.textContent = "Copied";
    setTimeout(() => (button.textContent = label), 1500);
  } catch {
    button.textContent = "Select and copy";
  }
}
