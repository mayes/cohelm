import { connect, copy } from "./link.js";

const $ = (id) => document.getElementById(id);

function row(name, tag, button) {
  const line = document.createElement("div");
  line.className = "row";
  line.dataset.agent = name;
  const label = document.createElement("span");
  label.className = "name";
  label.textContent = name;
  line.append(label);
  if (tag) {
    const note = document.createElement("span");
    note.className = "tag";
    note.textContent = tag;
    line.append(note);
  }
  if (button) line.append(button);
  return line;
}

function action(text, className, onClick) {
  const button = document.createElement("button");
  button.textContent = text;
  if (className) button.className = className;
  button.addEventListener("click", onClick);
  return button;
}

let details = {};
const send = connect((state) => {
  const { asking, connected, allowed, declined } = state.agents;
  $("status").hidden = true;
  $("lists").hidden = false;
  $("connect").hidden = false;

  $("asking-part").hidden = asking.length === 0;
  $("asking").replaceChildren(...asking.map((name) => row(name, "waiting for you")));

  $("allowed").replaceChildren(
    ...allowed.map((name) =>
      row(
        name,
        connected.includes(name) ? "connected" : "",
        action("Disconnect", "danger", () => send({ disconnect: name })),
      ),
    ),
  );
  $("none").hidden = allowed.length > 0 || asking.length > 0;

  $("declined-part").hidden = declined.length === 0;
  $("declined").replaceChildren(
    ...declined.map((name) => row(name, "", action("Allow", "", () => send({ allowAgain: name })))),
  );

  const update = state.update;
  $("update").hidden = !update;
  if (update) {
    $("update-version").textContent = update.version;
    $("current-version").textContent = state.version;
    $("update-download").href = update.download;
    $("update-notes").href = update.notes;
  }

  details = state.connect;
  for (const key of ["url", "claudeCode", "json"]) $(key).textContent = details[key];
}, {
  onMissing: () => {
    $("status").textContent = "Cohelm isn't running. Start Cohelm to manage Agents.";
  },
});

for (const button of document.querySelectorAll("[data-copy]")) {
  button.addEventListener("click", () => copy(details[button.dataset.copy] ?? "", button));
}
