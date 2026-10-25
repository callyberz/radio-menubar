// Radio — Logs window: Logs + Network tabs over the Rust diagnostics ring buffers.
// Entries arrive already redacted unless "Show sensitive values" is on (Rust does the masking).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Level = "debug" | "info" | "warn" | "error";
interface LogEntry {
  id: number;
  ts: number;
  level: Level;
  source: string;
  message: string;
}
type Pair = [string, string];
interface NetEntry {
  id: number;
  ts: number;
  source: string;
  kind: string;
  station: string | null;
  method: string;
  url: string;
  status: number | null;
  contentType: string | null;
  size: number | null;
  durationMs: number;
  reqHeaders: Pair[];
  respHeaders: Pair[];
  body: string | null;
  error: string | null;
}
interface Snapshot {
  logs: LogEntry[];
  net: NetEntry[];
  reveal: boolean;
  logFile: string | null;
}

const MAX_LOGS = 1000;
const MAX_NET = 500;

const state = {
  tab: "logs" as "logs" | "net",
  logs: [] as LogEntry[],
  net: [] as NetEntry[],
  levels: new Set<Level>(["debug", "info", "warn", "error"]),
  search: "",
  reveal: false,
  showSegments: false,
  expanded: new Set<string>(),
  selected: null as number | null,
  logFile: "" as string,
};

const $ = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id) as T;
const el = {
  tabLogs: $("tab-logs"),
  tabNet: $("tab-net"),
  paneLogs: $("pane-logs"),
  paneNet: $("pane-net"),
  logsBody: $("logs-body"),
  netBody: $("net-body"),
  netCols: $("net-cols"),
  netHead: $("net-head"),
  logsEmpty: $("logs-empty"),
  netEmpty: $("net-empty"),
  scrollLogs: $("scroll-logs"),
  scrollNet: $("scroll-net"),
  search: $<HTMLInputElement>("search"),
  levels: $("levels"),
  segToggle: $("seg-toggle"),
  showSegments: $<HTMLInputElement>("show-segments"),
  reveal: $<HTMLInputElement>("reveal"),
  detail: $("detail"),
  detailTitle: $("detail-title"),
  detailBody: $("detail-body"),
  count: $("count"),
  flash: $("flash"),
  logFile: $("logfile"),
};

// ------------------------------------------------------------------ formatting

const esc = (s: unknown) =>
  String(s ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ]!,
  );
const pad = (n: number, w = 2) => String(n).padStart(w, "0");
function fmtTime(ts: number) {
  const d = new Date(ts);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}
function fmtSize(b: number | null | undefined) {
  if (b == null) return "";
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(1)} MB`;
}
const failed = (e: NetEntry) =>
  e.error != null || e.status == null || e.status >= 400;
const shortType = (ct: string | null) => (ct ?? "").split(";")[0];

// ------------------------------------------------------------------ view model

function visibleLogs(): LogEntry[] {
  const q = state.search.trim().toLowerCase();
  return state.logs.filter(
    (e) =>
      state.levels.has(e.level) &&
      (!q || `${e.source} ${e.message}`.toLowerCase().includes(q)),
  );
}

interface Group {
  type: "group";
  key: string;
  station: string;
  ts: number;
  members: NetEntry[];
}
type NetRow = { type: "entry"; e: NetEntry; member: boolean } | Group;

function netMatches(e: NetEntry, q: string) {
  return (
    !q ||
    `${e.method} ${e.url} ${e.status ?? ""} ${e.contentType ?? ""} ${e.source} ${e.kind}`
      .toLowerCase()
      .includes(q)
  );
}

/** Segment requests are grouped per station per minute unless "Show individual segments". */
function netRows(): NetRow[] {
  const q = state.search.trim().toLowerCase();
  const rows: NetRow[] = [];
  const groups = new Map<string, Group>();
  for (const e of state.net) {
    if (e.kind === "segment" && !state.showSegments) {
      const key = `${e.station ?? "?"}|${Math.floor(e.ts / 60000)}`;
      let g = groups.get(key);
      if (!g) {
        g = {
          type: "group",
          key,
          station: e.station ?? "?",
          ts: e.ts,
          members: [],
        };
        groups.set(key, g);
        rows.push(g);
      }
      g.members.push(e);
    } else if (netMatches(e, q)) {
      rows.push({ type: "entry", e, member: false });
    }
  }
  const out: NetRow[] = [];
  for (const r of rows) {
    if (r.type === "group") {
      if (
        q &&
        !r.members.some((m) => netMatches(m, q)) &&
        !"segments".includes(q)
      )
        continue;
      out.push(r);
      if (state.expanded.has(r.key))
        for (const m of r.members)
          out.push({ type: "entry", e: m, member: true });
    } else out.push(r);
  }
  return out;
}

function groupSummary(g: Group) {
  const n = g.members.length;
  const counts = new Map<string, number>();
  for (const m of g.members) {
    const k = m.error ? "ERR" : String(m.status ?? "ERR");
    counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  const status =
    counts.size === 1
      ? `all ${[...counts.keys()][0]}`
      : [...counts].map(([k, c]) => `${k} ×${c}`).join(", ");
  const avg = Math.round(g.members.reduce((a, m) => a + m.durationMs, 0) / n);
  const total = g.members.reduce((a, m) => a + (m.size ?? 0), 0);
  return {
    text: `segments ×${n} · ${status} · avg ${avg} ms · ${fmtSize(total)}`,
    failed: g.members.some(failed),
    total,
  };
}

// ------------------------------------------------------------------ render

let pending = false;
function scheduleRender() {
  if (pending) return;
  pending = true;
  requestAnimationFrame(() => {
    pending = false;
    render();
  });
}

const atBottom = (s: HTMLElement) =>
  s.scrollHeight - s.scrollTop - s.clientHeight < 24;

function render() {
  const isLogs = state.tab === "logs";
  el.tabLogs.classList.toggle("on", isLogs);
  el.tabNet.classList.toggle("on", !isLogs);
  el.tabLogs.setAttribute("aria-selected", String(isLogs));
  el.tabNet.setAttribute("aria-selected", String(!isLogs));
  el.paneLogs.hidden = !isLogs;
  el.paneNet.hidden = isLogs;
  el.levels.hidden = !isLogs;
  el.segToggle.hidden = isLogs;
  el.reveal.checked = state.reveal;
  el.showSegments.checked = state.showSegments;
  for (const chip of el.levels.querySelectorAll<HTMLElement>(".chip"))
    chip.classList.toggle("on", state.levels.has(chip.dataset.level as Level));

  if (isLogs) renderLogs();
  else renderNet();
  el.logFile.textContent = state.logFile
    ? `Log file: ${state.logFile.replace(/^\/Users\/[^/]+/, "~")}`
    : "";
}

function renderLogs() {
  const stick = atBottom(el.scrollLogs);
  const rows = visibleLogs();
  el.logsBody.innerHTML = rows
    .map(
      (e) =>
        `<tr class="lv-${e.level}"><td class="mono dim">${fmtTime(e.ts)}</td><td class="lvl">${e.level}</td>` +
        `<td class="mono">${esc(e.source)}</td><td class="mono msg">${esc(e.message)}</td></tr>`,
    )
    .join("");
  el.logsEmpty.hidden = rows.length > 0;
  el.count.textContent = `${rows.length} of ${state.logs.length} log entries`;
  if (stick) el.scrollLogs.scrollTop = el.scrollLogs.scrollHeight;
}

// Full column set, or a compact one while the detail pane is open (so the URL stays readable).
type ColKey = "time" | "method" | "url" | "status" | "type" | "size" | "dur";
const COLS: Record<ColKey, { label: string; width?: number; num?: boolean }> = {
  time: { label: "Time", width: 104 },
  method: { label: "Method", width: 56 },
  url: { label: "URL" },
  status: { label: "Status", width: 56 },
  type: { label: "Type", width: 150 },
  size: { label: "Size", width: 74, num: true },
  dur: { label: "Time", width: 72, num: true },
};
const FULL: ColKey[] = ["time", "method", "url", "status", "type", "size", "dur"];
const COMPACT: ColKey[] = ["time", "url", "status", "size"];
let lastColsKey = "";

function renderNet() {
  const stick = atBottom(el.scrollNet);
  const rows = netRows();
  const cols = state.net.some((x) => x.id === state.selected) ? COMPACT : FULL;
  const colsKey = cols.join();
  if (colsKey !== lastColsKey) {
    lastColsKey = colsKey;
    el.netCols.innerHTML = cols.map((c) => `<col${COLS[c].width ? ` style="width:${COLS[c].width}px"` : ""} />`).join("");
    el.netHead.innerHTML = cols.map((c) => `<th${COLS[c].num ? ' class="num"' : ""}>${COLS[c].label}</th>`).join("");
  }
  const tr = (cells: Partial<Record<ColKey, string>>) => cols.map((c) => cells[c] ?? "<td></td>").join("");
  el.netBody.innerHTML = rows
    .map((r) => {
      if (r.type === "group") {
        const s = groupSummary(r);
        const open = state.expanded.has(r.key);
        return (
          `<tr class="group${s.failed ? " failed" : ""}" data-group="${esc(r.key)}">` +
          tr({
            time: `<td class="mono dim">${fmtTime(r.ts)}</td>`,
            method: `<td class="mono">GET</td>`,
            url: `<td class="url" title="${esc(s.text)}"><span class="caret${open ? " open" : ""}">▸</span><span class="tag">${esc(r.station)}</span>${esc(s.text)}</td>`,
            status: `<td class="mono">${s.failed ? "!" : ""}</td>`,
            type: `<td class="mono dim">audio segments</td>`,
            size: `<td class="mono num">${fmtSize(s.total)}</td>`,
          }) +
          `</tr>`
        );
      }
      const e = r.e;
      const cls = [
        failed(e) ? "failed" : "",
        e.kind === "redirect" ? "redirect" : "",
        r.member ? "member" : "",
        state.selected === e.id ? "selected" : "",
      ].join(" ");
      return (
        `<tr class="${cls}" data-id="${e.id}" title="${esc(e.url)}">` +
        tr({
          time: `<td class="mono dim">${fmtTime(e.ts)}</td>`,
          method: `<td class="mono">${esc(e.method)}</td>`,
          url: `<td class="mono url">${e.station && !r.member ? `<span class="tag">${esc(e.station)}</span>` : ""}${esc(e.url)}</td>`,
          status: `<td class="mono">${e.error ? "ERR" : (e.status ?? "")}</td>`,
          type: `<td class="mono dim">${esc(shortType(e.contentType))}</td>`,
          size: `<td class="mono num">${fmtSize(e.size)}</td>`,
          dur: `<td class="mono num">${e.durationMs} ms</td>`,
        }) +
        `</tr>`
      );
    })
    .join("");
  el.netEmpty.hidden = rows.length > 0;
  el.count.textContent = `${rows.length} rows · ${state.net.length} requests`;
  renderDetail();
  if (stick) el.scrollNet.scrollTop = el.scrollNet.scrollHeight;
}

function kvTable(pairs: Pair[]) {
  if (!pairs.length) return `<div class="dim">(none)</div>`;
  return `<div class="kv">${pairs.map(([k, v]) => `<div class="k">${esc(k)}</div><div class="v">${esc(v)}</div>`).join("")}</div>`;
}

function renderDetail() {
  const e = state.net.find((x) => x.id === state.selected);
  el.detail.hidden = !e;
  if (!e) return;
  el.detailTitle.textContent = `${e.method} ${e.url.replace(/^https?:\/\//, "")}`;
  const general: Pair[] = [
    ["URL", e.url],
    ["Method", e.method],
    ["Status", e.error ? `error: ${e.error}` : String(e.status ?? "")],
    ["Time", `${new Date(e.ts).toLocaleDateString()} ${fmtTime(e.ts)}`],
    ["Duration", `${e.durationMs} ms`],
    ["Size", fmtSize(e.size) || "?"],
    ["Content-Type", e.contentType ?? ""],
    ["Source", e.source],
    ["Kind", e.kind],
  ];
  if (e.station) general.push(["Station", e.station]);
  el.detailBody.innerHTML =
    `<h3>General</h3>${kvTable(general)}` +
    `<h3>Request headers</h3>${kvTable(e.reqHeaders)}` +
    `<h3>Response headers</h3>${kvTable(e.respHeaders)}` +
    `<h3>Response body${e.body ? " (first 2 KB)" : ""}</h3>` +
    (e.body
      ? `<pre class="body-pre">${esc(e.body)}</pre>`
      : `<div class="dim">${e.kind === "segment" ? "(binary audio)" : "(not captured)"}</div>`);
}

// ------------------------------------------------------------------ text / export

function visibleText(): string {
  if (state.tab === "logs")
    return visibleLogs()
      .map(
        (e) =>
          `${fmtTime(e.ts)}  ${e.level.toUpperCase().padEnd(5)}  ${e.source}  ${e.message}`,
      )
      .join("\n");
  return netRows()
    .map((r) => {
      if (r.type === "group")
        return `${fmtTime(r.ts)}  GET   [${r.station}] ${groupSummary(r).text}`;
      const e = r.e;
      return `${fmtTime(e.ts)}  ${e.method.padEnd(5)} ${e.error ? "ERR" : (e.status ?? "")}  ${shortType(e.contentType)}  ${fmtSize(e.size)}  ${e.durationMs} ms  ${e.url}${e.error ? `  (${e.error})` : ""}`;
    })
    .join("\n");
}

function visibleJson(): string {
  if (state.tab === "logs") return JSON.stringify(visibleLogs(), null, 2);
  const out: NetEntry[] = [];
  for (const r of netRows()) {
    if (r.type === "group") {
      if (!state.expanded.has(r.key)) out.push(...r.members);
    } else out.push(r.e);
  }
  return JSON.stringify(out, null, 2);
}

let flashTimer: number | undefined;
function flash(msg: string) {
  el.flash.textContent = msg;
  clearTimeout(flashTimer);
  flashTimer = window.setTimeout(() => (el.flash.textContent = ""), 3000);
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  }
}

// ------------------------------------------------------------------ events

function applySnapshot(s: Snapshot) {
  state.logs = s.logs;
  state.net = s.net;
  state.reveal = s.reveal;
  state.logFile = s.logFile ?? "";
  render();
}

function setTab(tab: "logs" | "net") {
  state.tab = tab;
  render();
}
el.tabLogs.addEventListener("click", () => setTab("logs"));
el.tabNet.addEventListener("click", () => setTab("net"));

el.search.addEventListener("input", () => {
  state.search = el.search.value;
  scheduleRender();
});
el.levels.addEventListener("click", (ev) => {
  const chip = (ev.target as HTMLElement).closest<HTMLElement>(".chip");
  if (!chip) return;
  const lv = chip.dataset.level as Level;
  if (state.levels.has(lv)) state.levels.delete(lv);
  else state.levels.add(lv);
  render();
});
el.showSegments.addEventListener("change", () => {
  state.showSegments = el.showSegments.checked;
  render();
});
el.reveal.addEventListener("change", async () => {
  // session-only: Rust keeps the flag in memory, so every launch starts redacted
  applySnapshot(
    await invoke<Snapshot>("diag_set_reveal", { reveal: el.reveal.checked }),
  );
  flash(
    state.reveal
      ? "Showing sensitive values (until the app quits)"
      : "Sensitive values hidden",
  );
});

el.netBody.addEventListener("click", (ev) => {
  const tr = (ev.target as HTMLElement).closest<HTMLElement>("tr");
  if (!tr) return;
  if (tr.dataset.group) {
    const k = tr.dataset.group;
    if (state.expanded.has(k)) state.expanded.delete(k);
    else state.expanded.add(k);
  } else if (tr.dataset.id) {
    const id = Number(tr.dataset.id);
    state.selected = state.selected === id ? null : id;
  }
  render();
});
$("detail-close").addEventListener("click", () => {
  state.selected = null;
  render();
});

$("btn-clear").addEventListener("click", async () => {
  await invoke("diag_clear", { which: state.tab });
  if (state.tab === "logs") state.logs = [];
  else {
    state.net = [];
    state.selected = null;
    state.expanded.clear();
  }
  render();
});
$("btn-copy").addEventListener("click", async () => {
  const text = visibleText();
  const n = text ? text.split("\n").length : 0;
  flash((await copyText(text)) ? `Copied ${n} rows` : "Copy failed");
});
$("btn-export").addEventListener("click", async () => {
  const d = new Date();
  const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
  try {
    const path = await invoke<string | null>("export_diag", {
      txt: visibleText() + "\n",
      json: visibleJson(),
      fileName: `radio-${state.tab === "logs" ? "logs" : "network"}-${stamp}.txt`,
    });
    if (path) flash(`Exported to ${path.replace(/^\/Users\/[^/]+/, "~")}`);
  } catch (e) {
    flash(`Export failed: ${e}`);
  }
});
$("btn-reveal").addEventListener("click", () =>
  invoke("reveal_log_file").catch((e) => flash(String(e))),
);

document.addEventListener("keydown", (e) => {
  if (
    (e.metaKey && e.key.toLowerCase() === "f") ||
    (e.key === "/" && document.activeElement !== el.search)
  ) {
    e.preventDefault();
    el.search.focus();
    el.search.select();
  } else if (e.key === "Escape" && state.selected != null) {
    state.selected = null;
    render();
  }
});

async function boot() {
  let lastLog = 0;
  let lastNet = 0;
  // subscribe first, then fill; drop duplicates by id
  await listen<LogEntry>("diag-log", (ev) => {
    if (ev.payload.id <= lastLog) return;
    lastLog = ev.payload.id;
    state.logs.push(ev.payload);
    if (state.logs.length > MAX_LOGS)
      state.logs.splice(0, state.logs.length - MAX_LOGS);
    if (state.tab === "logs") scheduleRender();
  });
  await listen<NetEntry>("diag-net", (ev) => {
    if (ev.payload.id <= lastNet) return;
    lastNet = ev.payload.id;
    state.net.push(ev.payload);
    if (state.net.length > MAX_NET)
      state.net.splice(0, state.net.length - MAX_NET);
    if (state.tab === "net") scheduleRender();
  });
  const snap = await invoke<Snapshot>("diag_snapshot");
  applySnapshot(snap);
  lastLog = Math.max(lastLog, ...snap.logs.map((e) => e.id), 0);
  lastNet = Math.max(lastNet, ...snap.net.map((e) => e.id), 0);
  if (new URLSearchParams(location.search).get("tab") === "net") setTab("net");
}

boot();
