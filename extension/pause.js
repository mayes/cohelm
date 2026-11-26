import { connect } from "./link.js";

const $ = (id) => document.getElementById(id);
const show = (id) => {
  for (const part of ["waiting", "prompt", "done"]) $(part).hidden = part !== id;
};

const KINDS = {
  send: ["Send", "This may send something in your name."],
  pay: ["Pay", "This may pay or buy something."],
  submit: ["Submit", "This may submit a form in your name."],
  delete: ["Delete", "This may delete something."],
};

// As with the Agent prompt, nothing has focus and the buttons wait a
// moment, so a key meant for another window can't answer it.
const SETTLE_MS = 1000;
let shown = false;

const send = connect((state) => {
  const pause = state.pause;
  if (pause) {
    const [label, may] = KINDS[pause.kind] ?? KINDS.submit;
    $("kind").textContent = label;
    $("agent").textContent = pause.agent;
    $("action").textContent = pause.action;
    $("where").hidden = !pause.site;
    $("site").textContent = pause.site ?? "";
    // What the Agent sends with the step, such as a script, as plain text.
    $("detail").hidden = !pause.detail;
    $("detail").textContent = pause.detail ?? "";
    $("may").textContent = may;
    show("prompt");
    if (!shown) {
      shown = true;
      for (const id of ["allow", "block"]) $(id).disabled = true;
      setTimeout(() => {
        for (const id of ["allow", "block"]) $(id).disabled = false;
      }, SETTLE_MS);
    }
  } else {
    show("done");
  }
}, {
  onMissing: () => {
    $("waiting").firstElementChild.textContent = "Cohelm isn't running.";
  },
});

const answer = (allow) => {
  send({ allow });
  show("done");
};
$("allow").addEventListener("click", () => answer(true));
$("block").addEventListener("click", () => answer(false));
