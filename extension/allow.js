import { connect } from "./link.js";

const $ = (id) => document.getElementById(id);
const show = (id) => {
  for (const part of ["waiting", "prompt", "done"]) $(part).hidden = part !== id;
};

// The prompt opens in front of whatever the person was doing, so nothing
// has focus and the buttons wait a moment: a key meant for another window
// can't answer it.
const SETTLE_MS = 1000;
let shown = false;

const send = connect((state) => {
  if (state.asking) {
    $("agent").textContent = state.asking;
    show("prompt");
    if (!shown) {
      shown = true;
      for (const id of ["allow", "decline"]) $(id).disabled = true;
      setTimeout(() => {
        for (const id of ["allow", "decline"]) $(id).disabled = false;
      }, SETTLE_MS);
    }
  } else {
    show("done");
  }
}, {
  onMissing: () => {
    $("waiting").firstElementChild.textContent =
      "Cohelm isn't running. Start Cohelm, then try again.";
  },
});

const answer = (allow) => {
  send({ allow });
  show("done");
};
$("allow").addEventListener("click", () => answer(true));
$("decline").addEventListener("click", () => answer(false));
