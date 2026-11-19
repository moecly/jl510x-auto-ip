const invoke = window.__TAURI__?.core?.invoke;
const listen = window.__TAURI__?.event?.listen;

const elements = {
  appState: document.querySelector("#appState"),
  appStateText: document.querySelector("#appStateText"),
  interfaceSelect: document.querySelector("#interface"),
  interfaceHint: document.querySelector("#interfaceHint"),
  configForm: document.querySelector("#configForm"),
  runButton: document.querySelector("#runButton"),
  stopButton: document.querySelector("#stopButton"),
  previewButton: document.querySelector("#previewButton"),
  refreshButton: document.querySelector("#refreshInterfaces"),
  baseIp: document.querySelector("#baseIp"),
  netmask: document.querySelector("#netmask"),
  gateway: document.querySelector("#gateway"),
  ttl: document.querySelector("#ttl"),
  indexOffset: document.querySelector("#indexOffset"),
  count: document.querySelector("#count"),
  intervalMs: document.querySelector("#intervalMs"),
  replyTimeoutMs: document.querySelector("#replyTimeoutMs"),
  sourceMac: document.querySelector("#sourceMac"),
  mapCount: document.querySelector("#mapCount"),
  mappingBody: document.querySelector("#mappingBody"),
  mappingHint: document.querySelector("#mappingHint"),
  statusCard: document.querySelector("#statusCard"),
  statusOrb: document.querySelector("#statusOrb"),
  statusTitle: document.querySelector("#statusTitle"),
  statusDetail: document.querySelector("#statusDetail"),
  sentCount: document.querySelector("#sentCount"),
  boardCount: document.querySelector("#boardCount"),
  activeInterface: document.querySelector("#activeInterface"),
  replyBody: document.querySelector("#replyBody"),
  replyHint: document.querySelector("#replyHint"),
  logList: document.querySelector("#logList"),
  clearLog: document.querySelector("#clearLog"),
  frameHex: document.querySelector("#frameHex"),
  frameMeta: document.querySelector("#frameMeta"),
};

const replies = new Map();
let busy = false;
let stopRequested = false;
let currentInterface = "";

function setAppState(state, text) {
  elements.appState.dataset.state = state;
  elements.appStateText.textContent = text;
}

function setRunState(state, title, detail, appText) {
  elements.statusOrb.dataset.state = state;
  elements.statusTitle.textContent = title;
  elements.statusDetail.textContent = detail;
  if (appText) {
    setAppState(state === "ok" ? "ok" : state, appText);
  }
}

function setBusy(value) {
  busy = value;
  elements.runButton.disabled = value;
  elements.previewButton.disabled = value;
  elements.refreshButton.disabled = value;
  elements.stopButton.disabled = !value;
}

function nowText() {
  const date = new Date();
  const pad = (value, size = 2) => String(value).padStart(size, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(
    date.getMilliseconds(),
    3,
  )}`;
}

function log(message, kind = "info") {
  const item = document.createElement("li");
  if (kind !== "info") {
    item.classList.add(`log-${kind}`);
  }
  const time = document.createElement("time");
  time.textContent = nowText();
  const text = document.createElement("span");
  text.textContent = message;
  item.append(time, text);
  elements.logList.append(item);
  while (elements.logList.children.length > 200) {
    elements.logList.firstElementChild.remove();
  }
  elements.logList.scrollTop = elements.logList.scrollHeight;
}

function parseIpv4(text) {
  const parts = String(text).trim().split(".");
  if (parts.length !== 4) {
    return null;
  }
  const bytes = [];
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) {
      return null;
    }
    const value = Number(part);
    if (value > 255) {
      return null;
    }
    bytes.push(value);
  }
  return (((bytes[0] << 24) >>> 0) + (bytes[1] << 16) + (bytes[2] << 8) + bytes[3]) >>> 0;
}

function formatIpv4(value) {
  return [
    (value >>> 24) & 0xff,
    (value >>> 16) & 0xff,
    (value >>> 8) & 0xff,
    value & 0xff,
  ].join(".");
}

function numberValue(input, name) {
  const value = Number(input.value);
  if (!Number.isFinite(value) || value < 0) {
    throw new Error(`${name} 必须是非负数字`);
  }
  return Math.trunc(value);
}

function readConfig() {
  const baseIp = elements.baseIp.value.trim();
  if (parseIpv4(baseIp) === null) {
    throw new Error(`起始 IP 格式不正确：${baseIp || "空"}`);
  }
  const netmask = elements.netmask.value.trim();
  if (parseIpv4(netmask) === null) {
    throw new Error(`子网掩码格式不正确：${netmask || "空"}`);
  }
  const gateway = elements.gateway.value.trim();
  if (parseIpv4(gateway) === null) {
    throw new Error(`网关格式不正确：${gateway || "空"}`);
  }

  const ttl = numberValue(elements.ttl, "TTL");
  if (ttl < 2 || ttl > 65535) {
    throw new Error("TTL 必须在 2…65535 之间");
  }
  const mapCount = numberValue(elements.mapCount, "映射行数");
  if (mapCount > 254) {
    throw new Error("映射行数必须 <= 254");
  }

  return {
    interface: elements.interfaceSelect.value || null,
    sourceMac: elements.sourceMac.value.trim() || null,
    ttl,
    indexOffset: Number(elements.indexOffset.value),
    baseIp,
    netmask,
    gateway,
    count: numberValue(elements.count, "发送帧数"),
    intervalMs: numberValue(elements.intervalMs, "帧间隔"),
    replyTimeoutMs: numberValue(elements.replyTimeoutMs, "回包等待"),
    mapCount,
  };
}

function renderMapping() {
  const base = parseIpv4(elements.baseIp.value);
  let mapCount = 0;
  try {
    mapCount = Math.min(numberValue(elements.mapCount, "映射行数"), 254);
  } catch {
    mapCount = 0;
  }
  elements.mappingBody.replaceChildren();

  if (base === null || mapCount === 0) {
    elements.mappingHint.textContent = base === null ? "起始 IP 无效" : "基于起始 IP";
    const row = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 2;
    cell.className = "empty-cell";
    cell.textContent = base === null ? "请输入有效 IP" : "—";
    row.append(cell);
    elements.mappingBody.append(row);
    return;
  }

  elements.mappingHint.textContent = `共 ${mapCount} 块板`;
  for (let index = 1; index <= mapCount; index += 1) {
    const ipValue = base + index - 1;
    if (ipValue > 0xffffffff) {
      break;
    }
    const row = document.createElement("tr");
    const board = document.createElement("td");
    board.textContent = String(index);
    const ip = document.createElement("td");
    ip.textContent = formatIpv4(ipValue);
    row.append(board, ip);
    elements.mappingBody.append(row);
  }
}

function interfaceLabel(iface) {
  const flags = [];
  if (iface.isUp) {
    flags.push("up");
  }
  if (iface.isLoopback) {
    flags.push("loopback");
  }
  const suffix = flags.length ? ` · ${flags.join(", ")}` : "";
  return `${iface.name} · ${iface.mac || "无 MAC"}${suffix}`;
}

async function loadInterfaces() {
  if (!invoke) {
    return;
  }
  const previous = elements.interfaceSelect.value;
  elements.interfaceSelect.disabled = true;
  try {
    const list = await invoke("list_interfaces");
    elements.interfaceSelect.replaceChildren();

    for (const iface of list) {
      const option = document.createElement("option");
      option.value = iface.name;
      option.textContent = interfaceLabel(iface);
      elements.interfaceSelect.append(option);
    }

    if (!list.length) {
      const option = document.createElement("option");
      option.value = "";
      option.textContent = "未发现网卡";
      elements.interfaceSelect.append(option);
    } else if (
      previous &&
      list.some((iface) => iface.name === previous)
    ) {
      elements.interfaceSelect.value = previous;
    } else {
      const usable = list.find(
        (iface) => iface.isUp && !iface.isLoopback && iface.mac,
      );
      elements.interfaceSelect.value = (usable || list[0]).name;
    }

    currentInterface = elements.interfaceSelect.value;
    elements.activeInterface.textContent = currentInterface || "—";
    const selected = list.find((iface) => iface.name === currentInterface);
    elements.interfaceHint.textContent = selected
      ? `${selected.description || "网卡"} · 原始以太网帧需要管理员权限`
      : "选择连接到菊花链的网卡，需要管理员权限。";
    log(`发现 ${list.length} 个网卡`, "ok");
  } catch (error) {
    log(`读取网卡失败：${String(error)}`, "error");
    setRunState("error", "无法读取网卡", String(error), "网卡读取失败");
  } finally {
    elements.interfaceSelect.disabled = false;
  }
}

function clearReplies() {
  replies.clear();
  renderReplies();
  elements.sentCount.textContent = "0";
  elements.boardCount.textContent = "0";
  elements.replyHint.textContent = "等待运行";
}

function statusClass(reply) {
  const text = String(reply.statusText || "unknown");
  return `status-${text.replace(/[^a-z-]/gi, "-")}`;
}

function renderReplies() {
  const rows = [...replies.values()].sort((left, right) => {
    if (left.index !== right.index) {
      return left.index - right.index;
    }
    return String(left.mac).localeCompare(String(right.mac));
  });

  elements.replyBody.replaceChildren();
  if (!rows.length) {
    const row = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 7;
    cell.className = "empty-cell";
    cell.textContent = "暂无回包";
    row.append(cell);
    elements.replyBody.append(row);
    return;
  }

  for (const reply of rows) {
    const row = document.createElement("tr");
    const cells = [
      reply.index,
      reply.mac,
      reply.oldIp,
      reply.newIp,
      reply.ttl,
    ];
    for (const value of cells) {
      const cell = document.createElement("td");
      cell.textContent = String(value);
      if (value === reply.mac || value === reply.oldIp || value === reply.newIp) {
        cell.className = "mono";
      }
      row.append(cell);
    }

    const badgeCell = document.createElement("td");
    const badge = document.createElement("span");
    badge.className = `badge ${statusClass(reply)}`;
    badge.textContent = reply.statusText;
    badgeCell.append(badge);
    row.append(badgeCell);

    const detail = document.createElement("td");
    detail.className = "detail";
    detail.textContent = reply.detail;
    detail.title = reply.detail;
    row.append(detail);
    elements.replyBody.append(row);
  }
}

function rememberReply(reply) {
  const previous = replies.get(reply.mac);
  if (!previous || previous.status === 4 || reply.status !== 4) {
    replies.set(reply.mac, reply);
  }
  renderReplies();
  elements.boardCount.textContent = String(replies.size);
  elements.replyHint.textContent = replies.size ? `${replies.size} 块板` : "暂无回包";
}

function applySummary(summary) {
  replies.clear();
  for (const board of summary.boards || []) {
    replies.set(board.mac, board);
  }
  renderReplies();
  elements.sentCount.textContent = String(summary.sent ?? 0);
  elements.boardCount.textContent = String(replies.size);
  elements.activeInterface.textContent = summary.interface || "—";
  elements.replyHint.textContent = replies.size ? `${replies.size} 块板` : "无回包";

  if (replies.size === 0) {
    setRunState(
      "warn",
      "没有收到回包",
      "检查网卡、链路和 TTL；也可以先用“预览帧”确认参数。",
      "无回包",
    );
    log("运行结束：没有收到板卡回包", "warn");
  } else {
    setRunState(
      "ok",
      "完成",
      `发送 ${summary.sent ?? 0} 帧，收到 ${replies.size} 块板回包。`,
      `${replies.size} 块板已响应`,
    );
    log(`运行完成：${replies.size} 块板，${summary.replyFrames ?? 0} 个回包帧`, "ok");
  }
}

async function previewFrame() {
  if (busy || !invoke) {
    return;
  }
  try {
    const preview = await invoke("dry_run", { config: readConfig() });
    elements.frameHex.textContent = preview.frameHex;
    elements.frameMeta.textContent = `${preview.interface} · ${preview.sourceMac}`;
    elements.activeInterface.textContent = preview.interface;
    elements.frameHex.scrollIntoView({ block: "center", behavior: "smooth" });
    log(`已生成预览帧：${preview.interface} / ${preview.sourceMac}`, "ok");
  } catch (error) {
    elements.frameHex.textContent = String(error);
    elements.frameMeta.textContent = "生成失败";
    log(`预览失败：${String(error)}`, "error");
    setRunState("error", "预览失败", String(error), "预览失败");
  }
}

async function stopRun() {
  if (!busy || !invoke) {
    return;
  }
  stopRequested = true;
  elements.stopButton.disabled = true;
  try {
    await invoke("stop_run");
    log("已请求停止，等待发送线程退出…", "warn");
  } catch (error) {
    log(`停止失败：${String(error)}`, "error");
  }
}

async function startRun() {
  if (busy || !invoke) {
    return;
  }

  let config;
  try {
    config = readConfig();
  } catch (error) {
    setRunState("error", "参数错误", String(error), "参数错误");
    log(String(error), "error");
    return;
  }

  clearReplies();
  stopRequested = false;
  setBusy(true);
  setRunState("running", "正在发送", "原始以太网帧已发出，等待板卡回包。", "运行中");
  log(
    `开始：${config.baseIp} / ${config.netmask} / ${config.gateway}，TTL ${config.ttl}，网卡 ${
      config.interface || "自动"
    }`,
  );

  try {
    const summary = await invoke("start_run", { config });
    applySummary(summary);
  } catch (error) {
    setRunState("error", "运行失败", String(error), "运行失败");
    log(`运行失败：${String(error)}`, "error");
  } finally {
    setBusy(false);
    if (stopRequested && elements.statusOrb.dataset.state === "running") {
      setRunState("warn", "已停止", "发送已停止，已收到的回包保留在表格中。", "已停止");
    }
  }
}

async function init() {
  elements.interfaceSelect.addEventListener("change", () => {
    currentInterface = elements.interfaceSelect.value;
    elements.activeInterface.textContent = currentInterface || "—";
  });
  elements.refreshButton.addEventListener("click", loadInterfaces);
  elements.previewButton.addEventListener("click", previewFrame);
  elements.stopButton.addEventListener("click", stopRun);
  elements.clearLog.addEventListener("click", () => {
    elements.logList.replaceChildren();
  });
  elements.configForm.addEventListener("submit", (event) => {
    event.preventDefault();
    startRun();
  });

  document.addEventListener("keydown", (event) => {
    const modifier = event.ctrlKey || event.metaKey;
    if (modifier && event.key.toLowerCase() === "p") {
      event.preventDefault();
      previewFrame();
    } else if (modifier && event.key === "Enter") {
      event.preventDefault();
      startRun();
    } else if (event.key === "Escape" && busy) {
      stopRun();
    }
  });

  for (const input of [elements.baseIp, elements.mapCount]) {
    input.addEventListener("input", renderMapping);
  }

  renderMapping();

  if (!invoke) {
    setRunState("error", "Tauri 未加载", "请通过 Tauri 启动 GUI，而不是直接打开 HTML。", "初始化失败");
    log("window.__TAURI__ 不可用", "error");
    return;
  }

  if (listen) {
    await listen("run-event", ({ payload }) => {
      if (!payload) {
        return;
      }
      if (payload.kind === "sent") {
        elements.sentCount.textContent = String(payload.sent);
        if (payload.sent === 1 || payload.sent % 10 === 0) {
          log(`已发送 ${payload.sent} 帧`);
        }
      } else if (payload.kind === "reply" && payload.reply) {
        rememberReply(payload.reply);
        log(
          `回包 #${payload.reply.index} ${payload.reply.mac} → ${payload.reply.newIp}（${payload.reply.statusText}）`,
          "ok",
        );
      }
    });
  } else {
    log("事件监听不可用，回包只在结束后显示", "warn");
  }

  log("GUI 初始化完成");
  await loadInterfaces();
  setRunState("ok", "就绪", "选择网卡并填写参数，然后发送请求。", "就绪");
}

init();
