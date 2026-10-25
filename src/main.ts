import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { load, type Store } from "@tauri-apps/plugin-store";
import {
  enable as enableAutostart,
  disable as disableAutostart,
  isEnabled as autostartEnabled,
} from "@tauri-apps/plugin-autostart";

// ------------------------------------------------------------------ model

type Code = "881" | "903";
interface StationInfo {
  code: Code;
  name: string;
}
const STATIONS: StationInfo[] = [
  { code: "881", name: "CR1 · 881" },
  { code: "903", name: "CR2 · 903" },
];
const stationInfo = (c: Code) => STATIONS.find((s) => s.code === c)!;

interface Programme {
  name: string;
  hosts: string;
  start: number; // unix seconds
  end: number;
}
type Schedule = Partial<Record<Code, Programme[]>>;

/** idle = paused by the user; the rest only apply while the user wants playback. */
type Conn = "idle" | "connecting" | "live" | "offline";
type LabelMode = "Icon + station" | "Icon only";

const SLEEP_OPTIONS = [0, 15, 30, 60, 90];
const RETRY_BACKOFF_S = [2, 5, 15]; // then every 15 s
const STALL_TIMEOUT_MS = 12_000;
const CONNECT_TIMEOUT_MS = 20_000;
const SCHEDULE_REFRESH_MS = 15 * 60_000;

const state = {
  station: "903" as Code,
  wantPlaying: false,
  conn: "idle" as Conn,
  volume: 0.65,
  sleepMinutes: 0,
  sleepRemaining: null as number | null,
  sleepOpen: false,
  launchAtLogin: false,
  labelMode: "Icon + station" as LabelMode,
  schedule: null as Schedule | null,
};

// ------------------------------------------------------------------ dom

const $ = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id) as T;
const el = {
  menu: $("menu"),
  badge: $("badge"),
  meta: $("meta"),
  programme: $("programme"),
  play: $("play"),
  volume: $("volume"),
  volumeFill: $("volume-fill"),
  stations: $("stations"),
  sleepRow: $("sleep-row"),
  sleepLabel: $("sleep-label"),
  sleepOptions: $("sleep-options"),
  loginRow: $("login-row"),
  loginSwitch: $("login-switch"),
  labelRow: $("label-row"),
  labelSwitch: $("label-switch"),
  logsRow: $("logs-row"),
  quitRow: $("quit-row"),
  audio: $<HTMLAudioElement>("audio"),
};

let store: Store | null = null;
const save = (key: string, value: unknown) => {
  store?.set(key, value).catch((e) => console.warn("store", e));
};

// ------------------------------------------------------------------ playback

let streamGen = 0;
let retryCount = 0;
let retryAt = 0;
let retryTimer: number | undefined;
let connectingSince = 0;
let lastProgress = { t: 0, at: 0 };

function teardownAudio() {
  const a = el.audio;
  a.pause();
  if (a.hasAttribute("src")) {
    a.removeAttribute("src");
    a.load(); // drop the connection and any stale buffer
  }
}

/** Sends player/UI events to the shared Rust log stream (Logs window + log file). */
type LogLevel = "debug" | "info" | "warn" | "error";
function plog(level: LogLevel, message: string, source = "player") {
  invoke("log_from_ui", { level, source, message }).catch(() => {});
}
const MEDIA_ERRORS: Record<number, string> = {
  1: "MEDIA_ERR_ABORTED",
  2: "MEDIA_ERR_NETWORK",
  3: "MEDIA_ERR_DECODE",
  4: "MEDIA_ERR_SRC_NOT_SUPPORTED",
};

function clearRetry() {
  retryAt = 0;
  if (retryTimer !== undefined) clearTimeout(retryTimer);
  retryTimer = undefined;
}

function setConn(c: Conn) {
  state.conn = c;
  render();
}

/** Always fetches a fresh stream URL, so resuming rejoins the live edge instead of old buffer. */
async function startStream() {
  const gen = ++streamGen;
  clearRetry();
  teardownAudio();
  connectingSince = Date.now();
  setConn("connecting");
  plog("info", `connecting to ${state.station}`);
  try {
    const url = await invoke<string>("prepare_stream", { code: state.station });
    if (gen !== streamGen || !state.wantPlaying) return;
    el.audio.src = url;
    el.audio.volume = state.volume;
    await el.audio.play();
  } catch (e) {
    if (gen !== streamGen) return; // superseded (station switch, pause, ...)
    console.warn("stream start failed:", e);
    plog("warn", `stream start failed (${state.station}): ${e}`);
    goOffline();
  }
}

function stopStream() {
  ++streamGen;
  clearRetry();
  teardownAudio();
  retryCount = 0;
  setConn("idle");
  plog("info", "paused by user");
}

function goOffline() {
  if (!state.wantPlaying) return;
  ++streamGen;
  teardownAudio();
  setConn("offline");
  if (retryAt) return;
  const delay =
    RETRY_BACKOFF_S[Math.min(retryCount, RETRY_BACKOFF_S.length - 1)] * 1000;
  retryCount++;
  plog("warn", `offline; retry #${retryCount} in ${delay / 1000} s`);
  retryAt = Date.now() + delay;
  retryTimer = window.setTimeout(runRetry, delay);
}

function runRetry() {
  if (!retryAt) return;
  clearRetry();
  if (state.wantPlaying) {
    plog("info", `retrying (attempt ${retryCount})`);
    startStream();
  }
}

function setPlaying(on: boolean) {
  if (
    on === state.wantPlaying &&
    (on ? state.conn !== "idle" : state.conn === "idle")
  )
    return;
  state.wantPlaying = on;
  if (on) {
    retryCount = 0;
    startStream();
  } else {
    stopStream();
  }
  render();
}

function selectStation(code: Code) {
  if (code === state.station && state.wantPlaying) return;
  plog("info", `station changed: ${state.station} → ${code}`);
  state.station = code;
  save("station", code);
  state.wantPlaying = true;
  retryCount = 0;
  startStream(); // switching starts the new stream right away
  render();
}

function setVolume(v: number) {
  state.volume = Math.max(0, Math.min(1, v));
  el.audio.volume = state.volume;
  render();
  saveVolumeSoon();
}
let volTimer: number | undefined;
function saveVolumeSoon() {
  clearTimeout(volTimer);
  volTimer = window.setTimeout(
    () => {
      save("volume", Math.round(state.volume * 1000) / 1000);
      plog("debug", `volume ${Math.round(state.volume * 100)}%`);
    },
    300,
  );
}

// audio element events
el.audio.addEventListener("playing", () => {
  if (!state.wantPlaying) return;
  retryCount = 0;
  lastProgress = { t: el.audio.currentTime, at: Date.now() };
  plog("info", `playing ${state.station} (live)`);
  setConn("live");
});
el.audio.addEventListener("waiting", () => {
  if (el.audio.hasAttribute("src")) plog("debug", "waiting (buffering)");
});
el.audio.addEventListener("stalled", () => {
  // WebKit fires this between HLS segment fetches too, so it's informational; the
  // watchdog decides when the stream is really stuck.
  if (el.audio.hasAttribute("src")) plog("debug", "stalled event (no data for a moment)");
});
el.audio.addEventListener("pause", () => {
  if (el.audio.hasAttribute("src")) plog("debug", "audio element paused");
});
el.audio.addEventListener("canplay", () => {
  if (state.wantPlaying && state.conn === "connecting" && !el.audio.paused)
    setConn("live");
});
el.audio.addEventListener("timeupdate", () => {
  if (el.audio.currentTime !== lastProgress.t)
    lastProgress = { t: el.audio.currentTime, at: Date.now() };
});
el.audio.addEventListener("error", () => {
  if (!el.audio.hasAttribute("src")) return; // our own teardown
  const err = el.audio.error;
  console.warn("audio error", err?.code, err?.message);
  plog(
    "error",
    `audio error ${err?.code ?? "?"} ${MEDIA_ERRORS[err?.code ?? 0] ?? ""}${err?.message ? `: ${err.message}` : ""}`,
  );
  goOffline();
});
el.audio.addEventListener("ended", () => {
  plog("warn", "stream ended unexpectedly");
  goOffline();
});

/** Runs every second (driven by Rust's `tick`, so it keeps running while the popover is hidden). */
function watchdog() {
  const now = Date.now();
  if (retryAt && now >= retryAt) runRetry();
  if (!state.wantPlaying) return;
  if (state.conn === "live" && now - lastProgress.at > STALL_TIMEOUT_MS) {
    console.warn("stream stalled");
    plog("warn", `no playback progress for ${STALL_TIMEOUT_MS / 1000} s`);
    goOffline();
  } else if (
    state.conn === "connecting" &&
    now - connectingSince > CONNECT_TIMEOUT_MS
  ) {
    console.warn("connect timeout");
    plog("warn", `connect timeout after ${CONNECT_TIMEOUT_MS / 1000} s`);
    goOffline();
  }
}

window.addEventListener("online", () => {
  if (state.wantPlaying && state.conn === "offline") {
    clearRetry();
    startStream();
  }
});

// ------------------------------------------------------------------ schedule

let scheduleFetchedAt = 0;
let scheduleFetching = false;

async function refreshSchedule() {
  if (scheduleFetching) return;
  scheduleFetching = true;
  try {
    state.schedule = await invoke<Schedule>("fetch_schedule");
    scheduleFetchedAt = Date.now();
  } catch (e) {
    console.warn("schedule unavailable:", e);
    scheduleFetchedAt = Date.now() - SCHEDULE_REFRESH_MS + 60_000; // retry in a minute
  } finally {
    scheduleFetching = false;
    render();
  }
}

function currentProgramme(code: Code): Programme | null {
  const list = state.schedule?.[code];
  if (!list) return null;
  const now = Date.now() / 1000;
  return list.find((p) => p.start <= now && now < p.end) ?? null;
}

const timeFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});
const fmtTime = (ts: number) => timeFmt.format(new Date(ts * 1000));

let lastMinute = -1;
function scheduleTick() {
  const now = Date.now();
  const minute = Math.floor(now / 60_000);
  const stale = now - scheduleFetchedAt > SCHEDULE_REFRESH_MS;
  const missing =
    state.schedule && STATIONS.some((s) => !currentProgramme(s.code));
  if (
    stale ||
    (missing && minute !== lastMinute && now - scheduleFetchedAt > 60_000)
  )
    refreshSchedule();
  if (minute !== lastMinute) {
    lastMinute = minute;
    render(); // programme names change on the minute
  }
}

// ------------------------------------------------------------------ render

const fmtMMSS = (s: number) =>
  `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

let lastTray = "";
function syncTray() {
  const playing = state.wantPlaying && state.conn === "live";
  const key = `${playing}|${state.station}`;
  if (key === lastTray) return;
  lastTray = key;
  invoke("set_playback_state", { playing, code: state.station }).catch(
    console.warn,
  );
}

function setSwitch(node: HTMLElement, on: boolean) {
  node.classList.toggle("on", on);
  node.setAttribute("aria-checked", String(on));
}

function buildStations() {
  el.stations.replaceChildren(
    ...STATIONS.map((s) => {
      const row = document.createElement("div");
      row.className = "station";
      row.dataset.code = s.code;
      row.setAttribute("role", "menuitemradio");
      row.innerHTML = `<div class="station-badge">${s.code}</div><div class="station-text"><span class="station-name">${s.name}</span><span class="station-now"></span></div>`;
      row.addEventListener("click", () => selectStation(s.code));
      return row;
    }),
  );
}

function buildSleepOptions() {
  el.sleepOptions.replaceChildren(
    ...SLEEP_OPTIONS.map((m) => {
      const row = document.createElement("div");
      row.className = "option";
      row.dataset.minutes = String(m);
      row.setAttribute("role", "menuitemradio");
      row.innerHTML = `<span class="check"></span><span>${m ? `${m} minutes` : "Off"}</span>`;
      row.addEventListener("click", () => pickSleep(m));
      return row;
    }),
  );
}

async function pickSleep(minutes: number) {
  state.sleepMinutes = minutes;
  state.sleepOpen = false;
  try {
    state.sleepRemaining =
      (await invoke<number | null>("set_sleep_timer", { minutes })) ?? null;
  } catch (e) {
    console.warn(e);
  }
  render();
}

function render() {
  const st = stationInfo(state.station);
  const prog = currentProgramme(state.station);
  const playingIntent = state.wantPlaying;

  // the round button is the only play/pause control in the popover
  el.play.classList.toggle("playing", playingIntent);
  el.play.setAttribute("aria-label", playingIntent ? "Pause" : "Play");

  // badge
  const [cls, text] = !playingIntent
    ? ["badge-paused", "PAUSED"]
    : state.conn === "live"
      ? ["badge-live", "LIVE"]
      : state.conn === "offline"
        ? ["badge-offline", "OFFLINE"]
        : ["badge-connecting", "CONNECTING…"];
  el.badge.className = `badge ${cls}`;
  el.badge.textContent = text;

  // meta line + programme
  el.meta.textContent = prog
    ? `${st.code} · ${fmtTime(prog.start)}–${fmtTime(prog.end)}`
    : st.name;
  if (playingIntent && state.conn === "offline") {
    el.programme.textContent = "Can’t reach stream — retrying";
    el.programme.classList.add("error");
  } else {
    el.programme.textContent = prog?.name ?? "";
    el.programme.classList.remove("error");
  }
  el.programme.title = prog?.hosts ?? "";

  // volume
  el.volumeFill.style.width = `max(22px, ${state.volume * 100}%)`;
  el.volume.setAttribute(
    "aria-valuenow",
    String(Math.round(state.volume * 100)),
  );
  el.volume.dataset.level =
    state.volume < 0.01 ? "0" : state.volume < 0.5 ? "1" : "2";

  // stations
  for (const row of el.stations.children as HTMLCollectionOf<HTMLElement>) {
    const code = row.dataset.code as Code;
    const selected = code === state.station;
    row.classList.toggle("selected", selected);
    row.setAttribute("aria-checked", String(selected));
    const p = currentProgramme(code);
    const now = row.querySelector(".station-now")!;
    now.textContent = p?.name ?? "";
    (now as HTMLElement).title = p
      ? `${p.name}${p.hosts ? ` — ${p.hosts}` : ""}`
      : "";
  }

  // sleep timer
  const remaining = state.sleepRemaining;
  el.sleepLabel.textContent =
    remaining != null ? `${fmtMMSS(remaining)} left` : "Off";
  el.sleepRow.classList.toggle("open", state.sleepOpen);
  el.sleepRow.setAttribute("aria-expanded", String(state.sleepOpen));
  el.sleepOptions.hidden = !state.sleepOpen;
  for (const row of el.sleepOptions.children as HTMLCollectionOf<HTMLElement>) {
    const active = Number(row.dataset.minutes) === state.sleepMinutes;
    row.querySelector(".check")!.textContent = active ? "✓" : "";
    row.setAttribute("aria-checked", String(active));
  }

  // settings
  setSwitch(el.loginSwitch, state.launchAtLogin);
  el.loginRow.setAttribute("aria-checked", String(state.launchAtLogin));
  const showLabel = state.labelMode !== "Icon only";
  setSwitch(el.labelSwitch, showLabel);
  el.labelRow.setAttribute("aria-checked", String(showLabel));

  syncTray();
  updateMediaSession(prog);
}

// ------------------------------------------------------------------ media session (media keys / Now Playing)

let lastMediaKey = "";
function updateMediaSession(prog: Programme | null) {
  const ms = navigator.mediaSession;
  if (!ms) return;
  const key = `${state.station}|${prog?.name}|${state.wantPlaying}`;
  if (key === lastMediaKey) return;
  lastMediaKey = key;
  try {
    if (typeof MediaMetadata !== "undefined") {
      ms.metadata = new MediaMetadata({
        title: prog?.name ?? stationInfo(state.station).name,
        artist: stationInfo(state.station).name,
        album: "Commercial Radio Hong Kong",
      });
    }
    ms.playbackState = state.wantPlaying ? "playing" : "paused";
  } catch (e) {
    console.warn("mediaSession", e);
  }
}
function setupMediaSession() {
  const ms = navigator.mediaSession;
  if (!ms) return;
  const handlers: [MediaSessionAction, () => void][] = [
    ["play", () => setPlaying(true)],
    ["pause", () => setPlaying(false)],
    ["stop", () => setPlaying(false)],
    ["nexttrack", () => selectStation(state.station === "881" ? "903" : "881")],
    [
      "previoustrack",
      () => selectStation(state.station === "881" ? "903" : "881"),
    ],
  ];
  for (const [action, fn] of handlers) {
    try {
      ms.setActionHandler(action, fn);
    } catch {
      /* unsupported action */
    }
  }
}

// ------------------------------------------------------------------ settings

async function refreshLaunchAtLogin() {
  try {
    state.launchAtLogin = await autostartEnabled();
  } catch (e) {
    console.warn("autostart", e);
  }
  render();
}

async function toggleLaunchAtLogin() {
  try {
    if (await autostartEnabled()) await disableAutostart();
    else await enableAutostart();
  } catch (e) {
    console.warn("autostart", e);
  }
  await refreshLaunchAtLogin(); // always show the real status
  save("launchAtLogin", state.launchAtLogin);
}

function setLabelMode(mode: LabelMode, notifyRust = true) {
  state.labelMode = mode;
  save("menubarLabel", mode);
  plog("info", `menu bar label: ${mode}`, "ui");
  if (notifyRust)
    invoke("set_label_mode", { showLabel: mode !== "Icon only" }).catch(
      console.warn,
    );
  render();
}

// ------------------------------------------------------------------ interactions

el.play.addEventListener("click", () => setPlaying(!state.wantPlaying));

let dragging = false;
const volFromEvent = (e: PointerEvent) => {
  const r = el.volume.getBoundingClientRect();
  setVolume((e.clientX - r.left) / r.width);
};
el.volume.addEventListener("pointerdown", (e) => {
  dragging = true;
  el.volume.setPointerCapture(e.pointerId);
  volFromEvent(e);
});
el.volume.addEventListener("pointermove", (e) => dragging && volFromEvent(e));
el.volume.addEventListener("pointerup", () => (dragging = false));
el.volume.addEventListener("pointercancel", () => (dragging = false));
el.volume.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const d = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : -e.deltaY;
    setVolume(state.volume + d / 400);
  },
  { passive: false },
);
el.volume.addEventListener("keydown", (e) => {
  if (e.key === "ArrowRight" || e.key === "ArrowUp")
    setVolume(state.volume + 0.05);
  else if (e.key === "ArrowLeft" || e.key === "ArrowDown")
    setVolume(state.volume - 0.05);
  else return;
  e.preventDefault();
});

el.sleepRow.addEventListener("click", () => {
  state.sleepOpen = !state.sleepOpen;
  render();
});
el.loginRow.addEventListener("click", () => toggleLaunchAtLogin());
el.labelRow.addEventListener("click", () =>
  setLabelMode(
    state.labelMode === "Icon only" ? "Icon + station" : "Icon only",
  ),
);
el.logsRow.addEventListener("click", () => invoke("open_logs"));
el.quitRow.addEventListener("click", () => invoke("quit_app"));

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    invoke("hide_popover");
  } else if (e.metaKey && e.altKey && e.code === "KeyL") {
    e.preventDefault();
    invoke("open_logs");
  } else if (e.metaKey && e.key.toLowerCase() === "q") {
    e.preventDefault();
    invoke("quit_app");
  } else if (e.key === " " && document.activeElement === document.body) {
    e.preventDefault();
    setPlaying(!state.wantPlaying);
  }
});
document.addEventListener("contextmenu", (e) => e.preventDefault());
// focus rings only while navigating with the keyboard
document.addEventListener("keydown", (e) => {
  if (e.key === "Tab") document.body.classList.add("kbd");
});
document.addEventListener("pointerdown", () => document.body.classList.remove("kbd"));

// Size the window to the content.
let lastHeight = 0;
new ResizeObserver(() => {
  const h = Math.ceil(el.menu.getBoundingClientRect().height);
  if (h > 0 && h !== lastHeight) {
    lastHeight = h;
    invoke("resize_popover", { height: h }).catch(console.warn);
  }
}).observe(el.menu);

// ------------------------------------------------------------------ boot

async function boot() {
  buildStations();
  buildSleepOptions();
  render();

  try {
    store = await load("settings.json", { autoSave: 200, defaults: {} });
    const [station, volume, labelMode] = await Promise.all([
      store.get<Code>("station"),
      store.get<number>("volume"),
      store.get<LabelMode>("menubarLabel"),
    ]);
    if (station === "881" || station === "903") state.station = station;
    if (typeof volume === "number" && volume >= 0 && volume <= 1)
      state.volume = volume;
    if (labelMode === "Icon only" || labelMode === "Icon + station")
      state.labelMode = labelMode;
  } catch (e) {
    console.warn("store unavailable", e);
  }
  el.audio.volume = state.volume;
  invoke("set_label_mode", {
    showLabel: state.labelMode !== "Icon only",
  }).catch(console.warn);
  render();

  await listen<{ sleepRemaining: number | null }>("tick", (e) => {
    state.sleepRemaining = e.payload.sleepRemaining ?? null;
    if (state.sleepRemaining == null && state.sleepMinutes !== 0)
      state.sleepMinutes = 0;
    watchdog();
    scheduleTick();
    render();
  });
  await listen("sleep-expired", () => {
    state.sleepMinutes = 0;
    state.sleepRemaining = null;
    setPlaying(false);
  });
  await listen<boolean>("label-mode-changed", (e) =>
    setLabelMode(e.payload ? "Icon + station" : "Icon only", false),
  );
  await listen("popover-shown", () => {
    state.sleepOpen = false;
    refreshLaunchAtLogin();
    render();
  });

  setupMediaSession();
  refreshLaunchAtLogin();
  refreshSchedule();

  const flags = await invoke<{ show: boolean; autoplay: boolean }>(
    "debug_flags",
  ).catch(() => ({ show: false, autoplay: false }));
  if (flags.autoplay) setPlaying(true);
}

boot();
