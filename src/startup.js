(function () {
  "use strict";

  const VALID_PHASES = new Set([
    "initializing",
    "checking",
    "approval",
    "starting",
    "retrying",
    "ready",
    "failed",
    "stopped",
  ]);
  const STEP_NAMES = ["初始化环境", "检查构建许可", "启动本地服务"];
  const TERMINAL_PHASES = new Set(["ready", "failed", "stopped"]);
  const ACTION_NAMES = new Set(["logs", "config", "retry", "background"]);
  const ICONS = {
    busy: '<circle cx="12" cy="12" r="9" opacity=".16"/><path d="M12 3a9 9 0 0 1 9 9"/>',
    approval: '<path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z"/><path d="M12 8v5m0 3v.1"/>',
    failed: '<circle cx="12" cy="12" r="9"/><path d="M12 7v6m0 4v.1"/>',
    ready: '<circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/>',
    stopped: '<circle cx="12" cy="12" r="9"/><path d="M8 12h8"/>',
    check: '<path d="m5 12 4 4L19 6"/>',
    dot: '<circle cx="12" cy="12" r="3" fill="currentColor" stroke="none"/>',
    pending: '<circle cx="12" cy="12" r="4"/>',
  };

  const elements = {
    shell: document.querySelector(".startup-shell"),
    title: document.getElementById("startup-title"),
    symbol: document.getElementById("state-symbol"),
    summary: document.getElementById("startup-summary"),
    connectionError: document.getElementById("connection-error"),
    steps: Array.from(document.querySelectorAll(".startup-step")),
    details: document.getElementById("startup-details"),
    detailsLabel: document.getElementById("details-label"),
    detailCount: document.getElementById("detail-count"),
    technicalDetail: document.getElementById("technical-detail"),
    readyNote: document.getElementById("ready-note"),
    elapsed: document.getElementById("elapsed-time"),
    actionError: document.getElementById("action-error"),
    buttons: Array.from(document.querySelectorAll("[data-action]")),
  };

  let currentSnapshot = null;
  let snapshotReceivedAt = 0;
  let latestRevision = -1;
  let elapsedTimer = null;
  let unlisten = null;
  let destroyed = false;
  let actionPending = false;
  let resizeObserver = null;
  let resizeFrame = null;
  let resizing = false;
  let desiredHeight = 278;
  let lastRequestedHeight = 0;

  function setIcon(element, name) {
    // 仅插入上面的静态图形常量；所有服务端文本仍使用 textContent。
    element.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[name]}</svg>`;
  }

  function setSymbol(phase) {
    const kind = ["approval", "failed", "ready", "stopped"].includes(phase) ? phase : "busy";
    elements.symbol.className = `state-symbol ${kind}`;
    setIcon(elements.symbol, kind);
  }

  async function flushResize() {
    const tauri = getTauri();
    if (!tauri || resizing || destroyed) return;
    resizing = true;
    try {
      while (!destroyed && desiredHeight !== lastRequestedHeight) {
        lastRequestedHeight = desiredHeight;
        await tauri.core.invoke("startup_resize", { height: desiredHeight, viewportHeight: window.innerHeight });
      }
    } catch (_) {
      // 关闭期间可能无法再调整大小；保留页面滚动作为窗口尺寸受限时的兜底。
    } finally {
      resizing = false;
    }
  }

  function scheduleResize() {
    if (destroyed || resizeFrame !== null) return;
    resizeFrame = requestAnimationFrame(() => {
      resizeFrame = null;
      desiredHeight = Math.ceil(elements.shell.getBoundingClientRect().height);
      void flushResize();
    });
  }

  function getTauri() {
    const candidate = window.__TAURI__;
    if (!candidate || !candidate.core || typeof candidate.core.invoke !== "function") {
      return null;
    }
    if (!candidate.event || typeof candidate.event.listen !== "function") {
      return null;
    }
    return candidate;
  }

  function redactSensitive(value) {
    const text = String(value == null ? "" : value);
    return text
      .replace(/(bearer\s+)[^\s,;]+/gi, "$1[已隐藏]")
      .replace(/((?:access[_-]?token|refresh[_-]?token|api[_-]?key|authorization|password|secret|token)\s*[:=]\s*)[^\s,;]+/gi, "$1[已隐藏]")
      .replace(/\b(?:sk|ghp|github_pat|xox[baprs])-[_a-z0-9-]+/gi, "[已隐藏]");
  }

  function isSnapshot(value) {
    return Boolean(
      value &&
        typeof value === "object" &&
        Number.isInteger(value.revision) &&
        Number.isInteger(value.attempt) &&
        VALID_PHASES.has(value.phase) &&
        Number.isInteger(value.step) &&
        value.step >= 0 &&
        value.step <= 3 &&
        typeof value.detail === "string" &&
        Number.isFinite(value.elapsedMs) &&
        value.elapsedMs >= 0 &&
        typeof value.testBuild === "boolean",
    );
  }

  function phaseTitle(phase) {
    if (phase === "initializing") return "正在准备启动环境…";
    if (phase === "checking") return "正在检查依赖…";
    if (phase === "approval") return "需要你的确认";
    if (phase === "ready") return "DSH 已就绪";
    if (phase === "failed") return "未能启动 DSH";
    if (phase === "stopped") return "启动已停止";
    if (phase === "retrying") return "正在重试启动…";
    return "正在启动本地服务…";
  }

  function failureSummary(detail) {
    if (/pnpm/i.test(detail) && /未找到|找不到|not found|ENOENT/i.test(detail)) return "未找到 pnpm 命令。";
    if (/node(?:\.js)?/i.test(detail) && /未找到|找不到|not found|ENOENT/i.test(detail)) return "未找到 Node.js 命令。";
    return "启动过程发生错误，详情已保留。";
  }

  function phaseSummary(snapshot) {
    switch (snapshot.phase) {
      case "initializing":
        return ["正在读取配置，检查外部运行环境。", "这通常只需要几秒钟。"];
      case "checking":
        return ["正在检查依赖的构建许可。", "首次启动可能需要下载依赖。"];
      case "approval":
        return ["发现需要运行构建脚本的依赖。", "请在系统对话框中确认是否允许。"];
      case "ready":
        return ["本地服务已启动，可以开始使用。", "此窗口将自动关闭，服务继续运行。"];
      case "failed":
        return [failureSummary(snapshot.detail), "请检查外部运行环境或启动配置。"];
      case "stopped":
        return ["启动过程已停止。", "你可以重新启动，或查看详情。"];
      case "retrying":
        return ["本地服务暂时不可用，正在自动重试。", "具体原因与重试间隔可在详情中查看。"];
      default:
        return ["正在等待 DSH 响应。", "首次启动可能需要下载依赖。"];
    }
  }

  function stepStatus(index, snapshot) {
    if (snapshot.phase === "ready") return "done";
    if (index < snapshot.step) return "done";
    if (index === snapshot.step && snapshot.phase === "stopped") return "stopped";
    if (index === snapshot.step && snapshot.phase === "failed") return "error";
    if (index === snapshot.step) return "active";
    return "pending";
  }

  function statusLabel(status) {
    if (status === "done") return "已完成";
    if (status === "active") return "进行中";
    if (status === "error") return "失败";
    if (status === "stopped") return "已停止";
    return "待处理";
  }

  function updateElapsed() {
    if (!currentSnapshot) return;
    const additionalMs = TERMINAL_PHASES.has(currentSnapshot.phase) ? 0 : performance.now() - snapshotReceivedAt;
    const seconds = Math.max(0, Math.floor((currentSnapshot.elapsedMs + additionalMs) / 1000));
    elements.elapsed.textContent = `${TERMINAL_PHASES.has(currentSnapshot.phase) ? "耗时" : "已用时"} ${seconds} 秒`;
  }

  function updateTimer() {
    if (elapsedTimer !== null) window.clearInterval(elapsedTimer);
    elapsedTimer = null;
    updateElapsed();
    if (!currentSnapshot || TERMINAL_PHASES.has(currentSnapshot.phase)) return;
    elapsedTimer = window.setInterval(updateElapsed, 500);
  }

  function renderDetails(snapshot) {
    elements.detailsLabel.textContent = snapshot.phase === "failed" ? "错误详情" : "启动详情";
    elements.detailCount.textContent = `${snapshot.step} / 3 已完成`;
    elements.detailCount.hidden = snapshot.phase === "failed" || snapshot.phase === "stopped";
    const detail = snapshot.detail || (snapshot.phase === "failed" ? "未提供错误详情，请查看日志。" : "");
    elements.technicalDetail.textContent = redactSensitive(detail);
    elements.technicalDetail.hidden = !detail;
    elements.steps.forEach((stepElement, index) => {
      const status = stepStatus(index, snapshot);
      stepElement.className = `startup-step ${status}`;
      const label = status === "active" && snapshot.phase === "approval" ? "等待确认" : statusLabel(status);
      stepElement.setAttribute("aria-label", `${STEP_NAMES[index]}：${label}`);
      stepElement.querySelector(".step-meta").textContent = label;
      const icon = { done: "check", active: "dot", error: "failed", stopped: "stopped", pending: "pending" }[status];
      setIcon(stepElement.querySelector(".step-symbol"), icon);
    });
  }

  function renderActions(snapshot) {
    const failed = snapshot.phase === "failed" || snapshot.phase === "stopped";
    const ready = snapshot.phase === "ready";
    const visible = {
      logs: true,
      config: failed,
      retry: failed,
      background: !failed && !ready,
    };
    elements.readyNote.hidden = !ready;
    elements.buttons.forEach((button) => {
      const action = button.dataset.action;
      button.hidden = !visible[action];
      button.disabled = actionPending;
      button.setAttribute("aria-busy", actionPending ? "true" : "false");
    });
  }

  function render(snapshot) {
    if (destroyed) return;
    if (!isSnapshot(snapshot)) {
      if (latestRevision < 0) {
        showConnectionError("启动器返回了无效的启动状态，无法安全显示进度。");
      }
      return;
    }
    if (snapshot.revision <= latestRevision) return;
    const newAttempt = !currentSnapshot || snapshot.attempt !== currentSnapshot.attempt;
    if (newAttempt) elements.details.open = false;
    latestRevision = snapshot.revision;
    currentSnapshot = snapshot;
    snapshotReceivedAt = performance.now();

    document.title = snapshot.testBuild ? "DSH Launcher（测试版）" : "DSH Launcher";
    elements.shell.classList.remove("startup-shell--connection-error");
    elements.connectionError.hidden = true;
    elements.details.hidden = false;
    elements.elapsed.hidden = false;
    elements.title.textContent = phaseTitle(snapshot.phase);
    elements.summary.replaceChildren(...phaseSummary(snapshot).map(line => {
      const span = document.createElement("span");
      span.textContent = line;
      return span;
    }));
    setSymbol(snapshot.phase);
    if (newAttempt) {
      elements.actionError.hidden = true;
      elements.actionError.textContent = "";
    }
    renderDetails(snapshot);
    renderActions(snapshot);
    updateTimer();
    scheduleResize();
  }

  function showConnectionError(message) {
    if (destroyed) return;
    elements.shell.classList.add("startup-shell--connection-error");
    elements.title.textContent = "无法连接到 DSH Launcher";
    elements.summary.textContent = "无法读取本地启动状态，请确认启动器正在运行后重试。";
    elements.connectionError.textContent = redactSensitive(message);
    elements.connectionError.hidden = false;
    elements.details.hidden = true;
    elements.elapsed.hidden = true;
    elements.readyNote.hidden = true;
    elements.buttons.forEach(button => { button.hidden = true; });
    setSymbol("failed");
    if (elapsedTimer !== null) window.clearInterval(elapsedTimer);
    elapsedTimer = null;
    scheduleResize();
  }

  function showActionError(message) {
    elements.actionError.textContent = redactSensitive(message);
    elements.actionError.hidden = false;
    scheduleResize();
  }

  async function invokeAction(action) {
    if (!ACTION_NAMES.has(action) || actionPending || destroyed) return;
    const tauri = getTauri();
    if (!tauri) {
      showConnectionError("无法连接到启动器操作接口。当前页面不会伪造启动成功状态。");
      return;
    }
    if (!currentSnapshot) return;
    if (action === "retry" && !["failed", "stopped"].includes(currentSnapshot.phase)) return;
    const actionAttempt = currentSnapshot.attempt;

    actionPending = true;
    renderActions(currentSnapshot || { phase: "starting" });
    elements.actionError.hidden = true;
    elements.actionError.textContent = "";
    try {
      const result = await tauri.core.invoke("startup_action", { action });
      if (!destroyed && currentSnapshot.attempt === actionAttempt && typeof result === "string" && result.trim()) {
        showActionError(result);
      }
    } catch (error) {
      if (!destroyed && currentSnapshot.attempt === actionAttempt) showActionError(error instanceof Error ? error.message : String(error));
    } finally {
      actionPending = false;
      if (!destroyed && currentSnapshot) renderActions(currentSnapshot);
    }
  }

  function cleanup() {
    if (destroyed) return;
    destroyed = true;
    if (resizeObserver) resizeObserver.disconnect();
    if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
    if (elapsedTimer !== null) {
      window.clearInterval(elapsedTimer);
      elapsedTimer = null;
    }
    const listener = unlisten;
    unlisten = null;
    if (typeof listener === "function") {
      try {
        listener();
      } catch (_) {
        // 页面正在离开时，监听器清理失败无需再显示 UI 错误。
      }
    }
  }

  async function initialize() {
    const tauri = getTauri();
    if (!tauri) {
      showConnectionError("未检测到 Tauri 主机 API。请从 DSH Launcher 打开此窗口，而不是直接打开 HTML 文件。");
      return;
    }

    try {
      const listener = await tauri.event.listen("startup-progress", (event) => {
        render(event && event.payload);
      });
      if (destroyed) {
        if (typeof listener === "function") listener();
        return;
      }
      unlisten = listener;

      const snapshot = await tauri.core.invoke("startup_snapshot");
      if (!isSnapshot(snapshot) && latestRevision < 0) {
        showConnectionError("启动器返回了无效的启动状态，无法安全显示进度。");
        return;
      }
      render(snapshot);
    } catch (error) {
      if (latestRevision < 0) showConnectionError(`无法读取启动状态：${redactSensitive(error instanceof Error ? error.message : String(error))}`);
    }
  }

  elements.buttons.forEach((button) => {
    button.addEventListener("click", () => invokeAction(button.dataset.action));
  });
  window.addEventListener("pagehide", cleanup, { once: true });
  window.addEventListener("beforeunload", cleanup, { once: true });
  elements.details.addEventListener("toggle", scheduleResize);
  resizeObserver = new ResizeObserver(scheduleResize);
  resizeObserver.observe(elements.shell);
  setSymbol("initializing");
  initialize();
})();
