/* Isolated visual simulation. No Tauri API, service requests, storage, or real permission handling. */
(() => {
  "use strict";
  const states = {
    initializing: { title: "正在准备启动环境…", lines: ["正在读取配置，检查外部运行环境。", "这通常只需要几秒钟。"], step: 0, seconds: 1 },
    checking: { title: "正在检查依赖…", lines: ["正在检查依赖的构建许可。", "首次启动可能需要下载依赖。"], step: 1, seconds: 3 },
    approval: { title: "需要你的确认", lines: ["发现需要运行构建脚本的依赖。", "请在系统对话框中确认是否允许。"], step: 1, seconds: 5 },
    starting: { title: "正在启动本地服务…", lines: ["正在等待 DSH 响应。", "首次启动可能需要下载依赖。"], step: 2, seconds: 8 },
    failed: { title: "未能启动 DSH", lines: ["未找到 pnpm 命令。", "请检查外部运行环境或启动配置。"], step: 2, seconds: 8 },
    ready: { title: "DSH 已就绪", lines: ["本地服务已启动，可以开始使用。", "此窗口将自动关闭，服务继续运行。"], step: 3, seconds: 9 },
  };
  const icons = {
    busy: '<circle cx="12" cy="12" r="9" opacity=".16"/><path d="M12 3a9 9 0 0 1 9 9"/>',
    approval: '<path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z"/><path d="M12 8v5m0 3v.1"/>',
    failed: '<circle cx="12" cy="12" r="9"/><path d="M12 7v6m0 4v.1"/>',
    ready: '<circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/>',
    check: '<path d="m5 12 4 4L19 6"/>',
    dot: '<circle cx="12" cy="12" r="3" fill="currentColor" stroke="none"/>',
    pending: '<circle cx="12" cy="12" r="4"/>',
    chevron: '<path d="m9 5 7 7-7 7"/>',
    arrow: '<path d="M7 17 17 7M7 7h10v10"/>',
  };
  const svg = (name, css = "") => `<svg class="${css}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name]}</svg>`;
  const stage = document.getElementById("preview-stage");
  const note = document.getElementById("preview-note");
  const play = document.getElementById("play-flow");
  const confirm = document.getElementById("confirm-approval");
  const params = new URLSearchParams(location.search);
  const media = matchMedia("(prefers-color-scheme: dark)");
  let phase = Object.hasOwn(states, params.get("state")) ? params.get("state") : "starting";
  let view = params.get("view") === "board" ? "board" : "interactive";
  let theme = ["light", "dark", "system"].includes(params.get("theme")) ? params.get("theme") : "light";
  let playing = false;
  let closed = false;
  let timer = null;
  let clock = null;
  let flowStartedAt = null;
  let epoch = 0;
  const windowOnly = params.get("view") === "window";
  if (windowOnly) document.body.classList.add("window-only");

  function applyTheme() {
    document.documentElement.dataset.theme = theme === "system" ? (media.matches ? "dark" : "light") : theme;
    document.getElementById("theme-control").value = theme;
  }

  function historyMarkup(state, selected) {
    return ["初始化环境", "检查构建许可", "启动本地服务"].map((label, i) => {
      const status = i < state.step ? "done" : i === state.step ? (selected === "failed" ? "error" : "active") : "pending";
      const symbol = status === "done" ? "check" : status === "error" ? "failed" : status === "active" ? "dot" : "pending";
      const meta = status === "done" ? "已完成" : status === "error" ? "失败" : status === "active" ? (selected === "approval" ? "等待确认" : "进行中") : "待处理";
      return `<li class="${status}"><span class="step-symbol">${svg(symbol)}</span><span>${label}</span><span class="step-meta">${meta}</span></li>`;
    }).join("");
  }

  function windowMarkup(selected, id) {
    const state = states[selected];
    const kind = ["approval", "failed", "ready"].includes(selected) ? selected : "busy";
    const terminal = selected === "failed" || selected === "ready";
    let actions = '<button class="button" type="button" data-action="background">后台继续</button>';
    if (selected === "failed") actions = '<button class="button" type="button" data-action="config">编辑配置</button><button class="button primary" type="button" data-action="retry">重试</button>';
    if (selected === "ready") actions = '<p class="ready-note">就绪后自动关闭</p>';
    return `<article class="app-window" data-state="${selected}" aria-labelledby="title-${id}">
      <div class="window-chrome"><div class="traffic-lights"><button type="button" class="traffic-light red" data-action="background" aria-label="模拟关闭启动窗口"></button><span class="traffic-light amber" aria-hidden="true"></span><span class="traffic-light green" aria-hidden="true"></span></div><span class="window-name">DSH Launcher</span></div>
      <div class="window-content">
        <div class="current-state"><span class="state-symbol ${kind}">${svg(kind)}</span><div class="state-copy" role="status" aria-live="polite" aria-atomic="true"><h2 id="title-${id}">${state.title}</h2><p>${state.lines.map(line => `<span>${line}</span>`).join("")}</p></div></div>
        <p class="elapsed" data-elapsed>${terminal ? "耗时" : "已用时"} ${state.seconds} 秒</p>
        <details class="startup-details"><summary>${svg("chevron", "disclosure-arrow")}<span>${selected === "failed" ? "错误详情" : "启动详情"}</span>${selected === "failed" ? "" : `<span class="detail-count">${state.step} / 3 已完成</span>`}</summary>
          <div class="detail-body"><ol class="history">${historyMarkup(state, selected)}</ol>
            ${selected === "failed" ? '<pre class="technical-detail">[模拟错误] spawn pnpm ENOENT\n未找到可执行文件。请确认 pnpm 已安装，并能在登录 shell 的 PATH 中找到。</pre>' : ""}
            ${selected === "approval" ? '<pre class="technical-detail">本窗口仅显示许可状态。依赖列表与授权操作仍在系统原生对话框中完成。</pre>' : ""}
            <button type="button" class="detail-link" data-action="logs">打开日志 ${svg("arrow")}</button>
          </div>
        </details>
        <footer class="window-actions">${actions}</footer>
      </div>
    </article>`;
  }

  function cancelTimers() {
    epoch += 1;
    clearTimeout(timer);
    clearInterval(clock);
    timer = null;
    clock = null;
  }

  function syncControls() {
    document.querySelectorAll("[data-phase]").forEach(button => button.setAttribute("aria-pressed", String(button.dataset.phase === phase && !closed && view !== "board")));
    document.querySelectorAll("[data-view]").forEach(button => button.setAttribute("aria-pressed", String(button.dataset.view === view)));
    confirm.hidden = !(view === "interactive" && phase === "approval" && !closed);
    play.textContent = playing ? "停止演示" : "播放完整流程";
  }

  function render() {
    stage.classList.toggle("board", view === "board");
    if (view === "board") {
      stage.innerHTML = ["starting", "approval", "failed"].map(state => windowMarkup(state, state)).join("");
      note.textContent = "三种状态使用同一套布局。窗口内的详情与操作可以点击；时间是固定的示例值。";
    } else if (closed) {
      stage.innerHTML = '<div class="closed-placeholder"><h2>启动窗口已关闭</h2><p>接入后：销毁 Webview，保留托盘和后台服务。<br>此处仅模拟界面消失，不代表实际资源释放。</p><button type="button" class="button" data-action="restore">重新查看窗口</button></div>';
      note.textContent = "这是关闭行为的模拟，没有启动或停止真实 DSH 服务。";
    } else {
      stage.innerHTML = windowMarkup(phase, "preview");
      note.textContent = phase === "approval" ? "这里不新增授权界面。可点击上方“模拟已确认许可”，继续体验启动过程。" : phase === "ready" && !playing ? "此处暂停在就绪状态供你查看；播放完整流程时，就绪 1.2 秒后模拟关闭。" : playing ? "正在播放模拟流程；在许可阶段暂停，等待你模拟确认。" : "点击阶段可静态查看；播放完整流程可体验阶段切换、许可等待和就绪后关闭。";
    }
    syncControls();
  }

  function runClock() {
    if (!playing || flowStartedAt === null) return;
    const ownEpoch = epoch;
    const update = () => {
      if (ownEpoch !== epoch || closed) return;
      const target = stage.querySelector("[data-elapsed]");
      const label = phase === "ready" || phase === "failed" ? "耗时" : "已用时";
      if (target) target.textContent = `${label} ${Math.floor((performance.now() - flowStartedAt) / 1000)} 秒`;
    };
    update();
    if (phase !== "ready" && phase !== "failed") clock = setInterval(update, 500);
  }

  function moveTo(next, isPlaying = false) {
    if (isPlaying && !playing) flowStartedAt = performance.now();
    if (!isPlaying) flowStartedAt = null;
    cancelTimers();
    phase = next;
    view = "interactive";
    closed = false;
    playing = isPlaying;
    render();
    runClock();
    const ownEpoch = epoch;
    const schedule = (ms, callback) => { timer = setTimeout(() => { if (ownEpoch === epoch) callback(); }, ms); };
    if (!playing) return;
    if (phase === "initializing") schedule(1600, () => moveTo("checking", true));
    if (phase === "checking") schedule(2000, () => moveTo("approval", true));
    if (phase === "starting") schedule(2600, () => moveTo("ready", true));
    if (phase === "ready") schedule(1200, closePreview);
  }

  function closePreview() {
    cancelTimers();
    playing = false;
    closed = true;
    view = "interactive";
    render();
  }

  function showSimulation(kind) {
    const title = document.getElementById("simulation-title");
    const description = document.getElementById("simulation-description");
    const content = document.getElementById("simulation-content");
    title.textContent = kind === "logs" ? "日志操作 · 模拟" : "配置操作 · 模拟";
    description.textContent = kind === "logs" ? "接入后会打开真实日志。下面仅是为了评审交互而提供的示例，没有读取本机日志。" : "接入后沿用现有“编辑配置”行为，调用系统编辑器。原型不会访问或修改 config.json，也不新增配置编辑器。";
    content.hidden = kind !== "logs";
    content.textContent = kind === "logs" ? "[示例] 初始化环境完成\n[示例] 检查构建许可完成\n[示例] 正在等待 DSH 响应…" : "";
    document.getElementById("simulation-dialog").showModal();
  }

  document.querySelectorAll("[data-phase]").forEach(button => button.addEventListener("click", () => moveTo(button.dataset.phase)));
  document.querySelectorAll("[data-view]").forEach(button => button.addEventListener("click", () => {
    cancelTimers();
    playing = false;
    closed = false;
    view = button.dataset.view;
    render();
  }));
  play.addEventListener("click", () => playing ? moveTo(phase) : moveTo("initializing", true));
  confirm.addEventListener("click", () => moveTo("starting", true));
  document.getElementById("theme-control").addEventListener("change", event => { theme = event.target.value; applyTheme(); });
  media.addEventListener("change", applyTheme);
  stage.addEventListener("click", event => {
    const button = event.target.closest("[data-action]");
    if (!button) return;
    switch (button.dataset.action) {
      case "background": closePreview(); break;
      case "restore": moveTo(phase); break;
      case "retry": moveTo("initializing", true); break;
      case "logs": showSimulation("logs"); break;
      case "config": showSimulation("config"); break;
    }
  });
  window.addEventListener("pagehide", cancelTimers, { once: true });
  applyTheme();
  render();
})();
