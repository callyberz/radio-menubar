# 📻 Provider Architecture · 電台來源架構

> [!NOTE]
> **Status: Proposal — not implemented yet.** · **狀態：提案，尚未實作。**
> Written 2026-10-24 against the current code in `src-tauri/src/` and `src/main.ts`.
> 根據 2026-10-24 時 `src-tauri/src/` 與 `src/main.ts` 的程式碼撰寫。

**Contents · 目錄**

1. [Why · 為甚麼要做](#1-why--為甚麼要做)
2. [Goals & non-goals · 目標與非目標](#2-goals--non-goals--目標與非目標)
3. [Where 881903 lives today · 現時 881903 的程式碼分佈](#3-where-881903-lives-today--現時-881903-的程式碼分佈)
4. [Proposed design · 建議設計](#4-proposed-design--建議設計)
5. [How playback flows · 播放流程](#5-how-playback-flows--播放流程)
6. [File-by-file plan · 逐檔修改計劃](#6-file-by-file-plan--逐檔修改計劃)
7. [Migration steps · 遷移步驟](#7-migration-steps--遷移步驟)
8. [Verification checklist · 驗收清單](#8-verification-checklist--驗收清單)
9. [Future providers · 日後可加入的來源](#9-future-providers--日後可加入的來源)
10. [Risks · 風險](#10-risks--風險)
11. [References · 參考資料](#11-references--參考資料)

---

## 1. Why · 為甚麼要做

Today the app only knows one broadcaster, 881903 (Commercial Radio Hong Kong). Its URLs, channel ids, stream-unlocking logic and schedule parsing are spread across several modules, and the station list is hard-coded again in the frontend. Adding any other source would mean touching all of them.

The goal is to move everything broadcaster-specific behind one **provider** interface, so a new source is one new module, and the rest of the app (popover, tray, proxy, logs window) never needs to know where audio comes from.

現時 App 只認識一個電台：881903（商業電台）。它的網址、頻道編號、解鎖串流的邏輯和節目表解析分散在幾個模組，而電台列表在前端又再寫死一次。想加入任何其他來源，就要逐一修改這些地方。

目標是把所有「電台專屬」的東西收進一個統一的 **Provider（來源）介面**。之後新增來源只需加一個模組，App 其餘部分（彈出選單、選單列圖示、本機代理、日誌視窗）完全不用理會聲音從哪裏來。

---

## 2. Goals & non-goals · 目標與非目標

| | English | 繁體中文 |
|---|---|---|
| ✅ | One `Provider` trait in Rust; 881903 becomes its first implementation | Rust 內一個 `Provider` trait，881903 成為第一個實作 |
| ✅ | Popover station list is built from data, not hard-coded | 彈出選單的電台列表由資料產生，不再寫死 |
| ✅ | The local proxy becomes generic (any provider that needs cookies/headers) | 本機代理變成通用（任何需要 cookie / header 的來源都可用） |
| ✅ | Behaviour stays **exactly the same** for the user after the refactor | 重構後使用者體驗 **完全不變** |
| ✅ | Logs keep the existing `provider:{id}` source tags | 日誌沿用現有的 `provider:{id}` 來源標籤 |
| ❌ | No runtime / third-party plugins yet (providers are compiled in) | 暫不支援執行時載入的第三方外掛（來源在編譯時內建） |
| ❌ | No "Add Station…" or Radio Browser search in this step | 這一步不做「新增電台…」或 Radio Browser 搜尋 |
| ❌ | No audio-only YouTube (see [§9](#9-future-providers--日後可加入的來源)) | 不做純音訊 YouTube（見 [§9](#9-future-providers--日後可加入的來源)） |

---

## 3. Where 881903 lives today · 現時 881903 的程式碼分佈

| File · 檔案 | What's 881903-specific · 881903 專屬內容 |
|---|---|
| `src-tauri/src/config.rs` | `PROVIDER_ID`, API URLs, `STREAM_HOST`, `REFERER`, and the `STATIONS` table (`881 → channel 1`, `903 → channel 2`) · 各 API 網址、串流主機、Referer 與電台對照表 |
| `src-tauri/src/resolver.rs` | Fetches the tokenised `playlist.js`, follows the redirect, collects CloudFront cookies, decodes the base64 / Crockford-base32 layers → `Session { client, jar, master }` · 取得帶 token 的 `playlist.js`、跟隨轉址、收集 CloudFront cookie、拆解 base64 / base32 → `Session` |
| `src-tauri/src/proxy.rs` | Routes `/live/{code}/…`, only forwards to `STREAM_HOST`, looks stations up with `config::station(code)` · 路由 `/live/{code}/…`，只轉發到 `STREAM_HOST`，用 `config::station` 找電台 |
| `src-tauri/src/schedule.rs` | Parses `/api/live/toolbar` → `{ "881": [...], "903": [...] }` via `config::station_by_channel` · 解析節目表 API |
| `src-tauri/src/lib.rs` | Commands `prepare_stream(code)` and `fetch_schedule()` call the modules above directly · `prepare_stream` 與 `fetch_schedule` 指令直接呼叫上述模組 |
| `src/main.ts` | A second, hard-coded `STATIONS` array (codes, titles) drives the popover · 前端另有一份寫死的 `STATIONS` 陣列 |

---

## 4. Proposed design · 建議設計

### 4.1 The `Provider` trait · `Provider` 介面

```rust
// src-tauri/src/providers/mod.rs

/// A source of live audio: a broadcaster, a directory, a user's own URL list…
pub trait Provider: Send + Sync {
    /// Stable id, also used in log tags and saved settings, e.g. "881903".
    fn id(&self) -> &'static str;

    /// Human-readable name, e.g. "Commercial Radio Hong Kong".
    fn name(&self) -> &str;

    /// Channels this provider offers, in display order.
    fn channels(&self) -> Vec<Channel>;

    /// Called every time playback starts, so "live" always means the live edge.
    fn resolve(&self, channel: &str) -> Result<StreamSource, String>;

    /// Optional programme data, keyed by channel code. Default: none.
    fn schedule(&self) -> Result<HashMap<String, Vec<Programme>>, String> {
        Ok(HashMap::new())
    }

    /// Hosts the local proxy may forward to for this provider. Default: none.
    fn proxy_hosts(&self) -> &[&'static str] {
        &[]
    }
}
```

### 4.2 Shared types · 共用型別

```rust
#[derive(Serialize, Clone)]
pub struct Channel {
    pub id: String,    // globally unique: "881903:903"
    pub code: String,  // badge + menu bar label: "903"
    pub title: String, // row title: "CR2 · 903"
}

/// How the webview should get the audio.
pub enum StreamSource {
    /// A URL <audio> can play as-is (plain Icecast, public HLS…).
    Direct { url: Url },
    /// Needs cookies or headers <audio> can't send → goes through the local proxy.
    Proxied { session: Arc<Session> },
}

/// Moved here unchanged from resolver.rs.
pub struct Session {
    pub client: Client,
    pub jar: Arc<Jar>,
    pub master: Url,
}
```

`Programme` (from `schedule.rs`) moves into `providers/mod.rs` unchanged.

`Programme`（來自 `schedule.rs`）原封不動搬到 `providers/mod.rs`。

### 4.3 Registry · 來源註冊表

```rust
pub struct Registry { providers: Vec<Arc<dyn Provider>> }

impl Registry {
    pub fn new() -> Self {
        Self { providers: vec![Arc::new(commercial_radio::CommercialRadio)] }
    }
    pub fn channels(&self) -> Vec<Channel> { /* all providers, in order */ }
    pub fn find(&self, channel_id: &str) -> Option<(Arc<dyn Provider>, String)> {
        /* "881903:903" → (881903 provider, "903") */
    }
}
```

| Concept · 概念 | English | 繁體中文 |
|---|---|---|
| Channel id · 頻道 ID | `"{provider}:{code}"`, e.g. `881903:903`. Unique across providers. | 格式 `"{來源}:{代號}"`，例如 `881903:903`，跨來源唯一。 |
| Badge code · 徽章代號 | Short text in the round badge and menu bar (`903`). | 圓形徽章及選單列顯示的短文字（`903`）。 |
| Registration · 註冊方式 | Compile-time list in `Registry::new()`. | 在 `Registry::new()` 編譯時列出。 |

### 4.4 Commands the frontend sees · 前端可用的指令

| Command · 指令 | Before · 之前 | After · 之後 |
|---|---|---|
| `list_channels` | — (hard-coded in `main.ts`) | **New.** Returns `Vec<Channel>` · **新增**，回傳所有頻道 |
| `prepare_stream` | `(code: "903")` | `(channel_id: "881903:903")` → local or direct URL · 回傳本機或直接網址 |
| `fetch_schedule` | 881903 only | Merged from every provider, keyed by channel id · 合併所有來源，以頻道 ID 為鍵 |
| `set_playback_state` | `(playing, code)` | `(playing, channel_id)`; tray shows that channel's `code` · 選單列顯示該頻道的 `code` |

### 4.5 Generic proxy · 通用代理

- Routes change from `/live/{code}/…` to `/live/{provider}/{code}/…`.
- The allow-list comes from `provider.proxy_hosts()` instead of the single `STREAM_HOST` constant.
- Playlist rewriting (`rewrite_playlist`) and the network log recording stay exactly as they are.

- 路由由 `/live/{code}/…` 改為 `/live/{provider}/{code}/…`。
- 可轉發的主機改由 `provider.proxy_hosts()` 提供，不再只有單一 `STREAM_HOST` 常數。
- 播放清單改寫（`rewrite_playlist`）和網絡日誌記錄保持不變。

### 4.6 Saved settings migration · 已儲存設定的遷移

The store currently saves `station: "903"`. On first launch after the refactor, a bare code is mapped to `881903:{code}` and saved back. Unknown values fall back to the first channel.

目前設定檔儲存的是 `station: "903"`。重構後首次啟動時，舊的純代號會轉成 `881903:{code}` 並寫回；無法辨認的值則改用第一個頻道。

---

## 5. How playback flows · 播放流程

```mermaid
sequenceDiagram
    autonumber
    participant UI as Popover (main.ts)<br/>彈出選單
    participant Cmd as Tauri commands<br/>指令
    participant Reg as Registry<br/>來源註冊表
    participant P as Provider (881903)
    participant Px as Local proxy<br/>本機代理
    participant A as &lt;audio&gt;

    UI->>Cmd: list_channels()
    Cmd->>Reg: channels()
    Reg-->>UI: [881903:881, 881903:903]
    UI->>Cmd: prepare_stream("881903:903")
    Cmd->>Reg: find("881903:903")
    Reg->>P: resolve("903")
    alt needs cookies · 需要 cookie
        P-->>Px: StreamSource::Proxied(session)
        Px-->>UI: http://127.0.0.1:PORT/live/881903/903/playlist.m3u8
    else plain URL · 普通網址
        P-->>UI: StreamSource::Direct(url)
    end
    UI->>A: src = url, play()
    A->>Px: playlist + segments (proxied case)
```

---

## 6. File-by-file plan · 逐檔修改計劃

```text
src-tauri/src/
├── providers/
│   ├── mod.rs              ← NEW: Provider trait, Channel, StreamSource, Session, Programme, Registry
│   └── commercial_radio/
│       ├── mod.rs          ← NEW: impl Provider for CommercialRadio
│       ├── config.rs       ← MOVED from src/config.rs
│       ├── resolver.rs     ← MOVED from src/resolver.rs (Session type moves to providers/mod.rs)
│       └── schedule.rs     ← MOVED from src/schedule.rs
├── proxy.rs                ← CHANGED: /live/{provider}/{code}/…, hosts from provider
├── lib.rs                  ← CHANGED: Registry in app state; new list_channels; updated commands
└── (diag.rs, net.rs, redact.rs, tray.rs, popover_monitors.rs — unchanged · 不變)

src/main.ts                 ← CHANGED: STATIONS comes from list_channels(); station ids are channel ids
```

> [!TIP]
> Rust module names can't start with a digit, so the folder is `commercial_radio/` while the provider **id** stays `"881903"`. That keeps log tags (`provider:881903`) and saved settings stable.
>
> Rust 模組名稱不能以數字開頭，所以資料夾叫 `commercial_radio/`，但來源 **ID** 仍然是 `"881903"`，日誌標籤（`provider:881903`）和已儲存設定都不受影響。

---

## 7. Migration steps · 遷移步驟

Each step builds and runs on its own, so it can stop at any point without breaking the app.

每一步完成後都能獨立編譯和運行，隨時可以停下而不會弄壞 App。

| # | English | 繁體中文 |
|---|---|---|
| 1 | Add `providers/mod.rs` with the trait and types. Nothing uses it yet. | 新增 `providers/mod.rs`（介面與型別），暫時未被使用。 |
| 2 | Move `config.rs`, `resolver.rs`, `schedule.rs` into `providers/commercial_radio/` and implement `Provider`. | 把三個檔案搬進 `providers/commercial_radio/` 並實作 `Provider`。 |
| 3 | Put a `Registry` in app state; route `prepare_stream` / `fetch_schedule` through it (still accepting old codes). | 在 app state 加入 `Registry`，讓兩個指令經它轉發（暫時仍接受舊代號）。 |
| 4 | Make the proxy generic (`/live/{provider}/{code}/…`, provider host allow-list). | 把代理改成通用版本。 |
| 5 | Add `list_channels`; frontend builds the station list from it; switch to channel ids. | 新增 `list_channels`，前端改用它產生電台列表並轉用頻道 ID。 |
| 6 | Saved-settings migration (`"903"` → `"881903:903"`); remove old-code fallbacks. | 遷移已儲存設定，移除舊代號的相容處理。 |
| 7 | Update README and this doc's status to **Implemented**. | 更新 README，並把本文件狀態改為 **已實作**。 |

---

## 8. Verification checklist · 驗收清單

- [ ] `pnpm tauri build` passes; no TypeScript errors, no clippy warnings · 編譯通過，沒有 TS 錯誤或 clippy 警告
- [ ] Existing unit tests pass (decoder, redaction) and the live test still resolves both stations · 現有單元測試及實際連線測試通過
- [ ] Both stations reach **LIVE**; switching station starts the new stream at once · 兩台都能達到 **LIVE**，轉台即時播放
- [ ] Real programme names still appear · 仍顯示真實節目名稱
- [ ] OFFLINE state and 2 s / 5 s / 15 s retries still work · 離線狀態和重試間隔正常
- [ ] Sleep timer, Launch at Login, "Show Station in Menu Bar" unchanged · 睡眠計時、登入時啟動、選單列顯示電台不變
- [ ] Popover still closes on outside click, tray click and Escape · 點外面、點圖示、按 Esc 仍會關閉彈出選單
- [ ] Logs window: entries still tagged `provider:881903`; secrets still redacted · 日誌仍標記 `provider:881903`，敏感資料仍被遮蔽
- [ ] An old saved `station: "903"` is migrated to `881903:903` · 舊設定正確遷移

---

## 9. Future providers · 日後可加入的來源

| Source · 來源 | Fit · 適合度 | Effort · 工作量 | Notes · 備註 |
|---|---|---|---|
| **Direct stream URL** · 直接串流網址 (Icecast / SHOUTcast / `.mp3` / `.aac` / `.m3u8`) | ⭐⭐⭐ | Small · 小 | `StreamSource::Direct`; covers thousands of stations · 一個型別就涵蓋數以千計的電台 |
| **Radio Browser** directory · 電台目錄 | ⭐⭐⭐ | Small · 小 | Free open API, ~50k stations; in-app search · 免費開放 API，約五萬個電台，可在 App 內搜尋 |
| **Podcasts (RSS)** · 播客 | ⭐⭐ | Medium · 中 | Not live: needs episode list + resume position · 非直播，需要集數列表和續播位置 |
| **YouTube** | ⭐ | — | Policies forbid separating audio from video; the official player must stay visible (≥ 200×200 px). Only a small visible embed window is compliant · 政策禁止分離影音，官方播放器須可見（至少 200×200 px），只有可見的嵌入視窗才合規 |

> [!WARNING]
> Any provider that unwraps a site's obfuscated stream URL (as 881903 does today) sits in a grey area and breaks when the site changes. Prefer sources with public, documented streams.
>
> 任何需要拆解網站混淆串流網址的來源（例如現時的 881903）都屬灰色地帶，網站一改就會失效。應優先選擇有公開、有文件說明的串流來源。

---

## 10. Risks · 風險

| Risk · 風險 | Mitigation · 應對 |
|---|---|
| Regressions in playback during the move · 搬遷時播放功能退步 | Small steps (§7), each built and run · 小步進行，每步都編譯及實測 |
| Lost saved station after upgrade · 升級後遺失已選電台 | Explicit migration + fallback to first channel · 明確遷移並設後備 |
| Proxy forwarding to unexpected hosts · 代理轉發到非預期主機 | Per-provider host allow-list, deny by default · 每個來源各自的主機白名單，預設拒絕 |
| Over-engineering for one provider · 只有一個來源卻過度設計 | Compile-time registry only; no plugin loader yet · 只做編譯時註冊，不做外掛載入器 |

---

## 11. References · 參考資料

- Tauri v2 — Calling Rust from the frontend: <https://v2.tauri.app/develop/calling-rust/>
- Tauri v2 — Logging plugin: <https://v2.tauri.app/plugin/logging/>
- Radio Browser API: <https://api.radio-browser.info/>
- YouTube API Services — Developer Policies: <https://developers.google.com/youtube/terms/developer-policies>
- YouTube IFrame Player API: <https://developers.google.com/youtube/iframe_api_reference>
- Current implementation notes · 現有實作說明: [`../README.md`](../README.md), [`../src-tauri/src/config.rs`](../src-tauri/src/config.rs)
