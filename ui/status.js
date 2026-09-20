// The window is driven entirely from Rust: it renders one payload and reports
// which button was pressed.
const { core, event } = window.__TAURI__;
const { getCurrentWindow, LogicalSize } = window.__TAURI__.window;

const nodes = {
  title: document.getElementById("title"),
  detail: document.getElementById("detail"),
  spinner: document.getElementById("spinner"),
  buttons: document.getElementById("buttons"),
  retry: document.getElementById("retry"),
  logs: document.getElementById("logs"),
  close: document.getElementById("close"),
};

// The detail text decides how tall the window has to be.
function fitWindow() {
  const height = Math.ceil(document.querySelector(".card").getBoundingClientRect().height);
  getCurrentWindow().setSize(new LogicalSize(420, height));
}

function render(payload) {
  if (!payload) return;
  const failed = payload.kind === "failure";
  document.body.classList.toggle("failure", failed);
  nodes.title.textContent = payload.title;
  nodes.detail.textContent = payload.detail;
  nodes.spinner.hidden = failed;
  nodes.buttons.hidden = !failed;
  nodes.retry.textContent = payload.retry;
  nodes.logs.textContent = payload.logs;
  nodes.close.textContent = payload.close;
  requestAnimationFrame(fitWindow);
}

for (const action of ["retry", "logs", "close"]) {
  nodes[action].addEventListener("click", () => core.invoke("status_action", { action }));
}

document.addEventListener("keydown", (pressed) => {
  if (pressed.key === "Escape") core.invoke("status_action", { action: "close" });
  if (pressed.key === "Enter" && !nodes.buttons.hidden) core.invoke("status_action", { action: "retry" });
});

event.listen("status", (message) => render(message.payload));
// The first payload may predate this listener, so ask for the current one.
core.invoke("status_ready").then(render);
