import { connect } from "./link.js";

const $ = (id) => document.getElementById(id);
const show = (id) => {
  for (const part of ["waiting", "prompt", "done"]) $(part).hidden = part !== id;
};

const send = connect((state) => {
  if (state.asking) {
    $("agent").textContent = state.asking;
    show("prompt");
    $("allow").focus();
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
