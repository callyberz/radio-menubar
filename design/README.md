# Handoff: Radio Menu Bar App (881 / 903)

## Overview
A native macOS menu bar app (no Dock icon) that plays the live radio streams from https://www.881903.com/live/881 and https://www.881903.com/live/903. It lives in the top-right menu bar as a status item. Clicking the item opens a plain native-style menu with playback controls, a station switcher, a sleep timer, launch at login and quit.

## About the Design Files
`Radio Menu Bar.dc.html` is a **design reference made in HTML**. It shows how the app should look and behave; it is not production code. Rebuild it as a native app: **Swift + SwiftUI using `MenuBarExtra` (macOS 13+)**, or AppKit `NSStatusItem` + `NSMenu` if you need custom views. Use `AVPlayer` for audio.

Open the HTML in a browser to try it. The menu starts open; click the status item to toggle it.

## Fidelity
**High-fidelity for structure and behaviour.** Use native macOS controls and system colours (accent colour, menu highlight, system font) rather than copying the HTML's colour values. The goal is for it to feel exactly like a built-in menu such as Wi-Fi or Sound.

## Requirements
1. Menu bar only (`LSUIElement = YES`, no Dock icon, no main window).
2. Plays live audio for 881 (CR1) and 903 (CR2).
3. Status item shows **icon + station code** ("903"). A setting allows icon only.
4. The menu contains: play/pause, live indicator, current programme name, volume, station switcher, sleep timer, launch at login, quit.
5. English UI.
6. Remember the last station, volume and launch-at-login setting between launches.

## Stream Sources (TO DO: confirm)
The audio URLs are not documented in the design. Open the live pages in a browser, use Web Inspector → Network, filter for `m3u8` / `aac` / `mp3`, and note the stream URLs for 881 and 903. They are likely HLS (`.m3u8`), which `AVPlayer` plays directly. URLs may be tokenised or expire; if so, fetch the live page or its API to get a fresh URL each time playback starts. Respect the broadcaster's terms of use.

Programme names: the prototype uses a made-up schedule. Use the site's real schedule or now-playing data if it's available (inspect network calls on the live page). Otherwise hide the programme line.

## Screens / Views

### 1. Status item (menu bar)
- Icon: 3 vertical bars (2pt wide, 11pt tall, 2pt gap) acting as a level meter. When playing, the bars bounce (scaleY 0.35→1, 0.8s / 0.95s / 1.1s, staggered 0.12s, ease-in-out, looping). When paused, all bars sit still at 35% height. A template SF Symbol is fine (`waveform` with `.variableColor` effect while playing, `waveform` static while paused).
- Label: station code, e.g. "903", SF semibold 13pt, tabular numerals.
- If a sleep timer is running, show the remaining time after the label ("29:41", 11pt, 60% opacity).
- Highlighted background while the menu is open (system default).

### 2. Menu (width ~300pt, native menu material, 12pt corner radius)
Top to bottom:
1. **Header row:** "Radio" (bold 13pt) on the left; a toggle switch on the right that plays or pauses (like Wi-Fi's on/off switch).
2. **Now playing block** (not clickable):
   - Badge: red (#E5342B) "LIVE" pill (10pt bold, white, with a pulsing white dot) while playing; outlined grey "PAUSED" pill while paused.
   - Next to the badge: "903 · 10:00–13:00" (11pt, secondary colour).
   - Programme name: 15pt semibold, e.g. "Non-Stop Hits".
3. **Controls row:** round 30pt play/pause button (8% black fill) + volume slider (22pt tall rounded track, white fill, speaker glyph on the left), like the Sound menu in Control Center.
4. Separator.
5. **Section header "Stations"** (12pt semibold, secondary colour).
6. **Station rows ×2** (hover highlight = rounded 6pt, 8% black):
   - 28pt circle badge with the code ("881" / "903", 10pt bold). Selected: accent fill + white text. Not selected: 10% black fill.
   - Title: "CR1 · 881" / "CR2 · 903" (13pt medium).
   - Subtitle: that station's current programme (11pt, secondary).
   - Click → switch to that station and start playing.
7. Separator.
8. **Sleep Timer ›** submenu. The right side shows "Off" or "mm:ss left". Options: Off, 15, 30, 60, 90 minutes, with a checkmark on the active one.
9. **Launch at Login** with a small switch (26×15pt).
10. Separator.
11. **Quit Radio** with "⌘Q" on the right.

## Interactions & Behavior
- Header switch and round button both toggle play/pause; they always show the same state.
- Switching station stops the current stream and starts the new one immediately (playing = true).
- Volume: 0–1, applied to `AVPlayer.volume`, saved.
- Sleep timer: picking N minutes sets an end time. A 1s tick updates the label. When it runs out, pause playback and set the timer back to Off. Picking Off cancels it.
- Launch at Login: `SMAppService.mainApp.register()` / `.unregister()` (macOS 13+). Show the real status.
- Quit: `NSApp.terminate`.
- Live means live: on resume after pause, rejoin the live edge (recreate the item or seek to the end) rather than playing old buffered audio.
- Optional: support media keys / Now Playing via `MPRemoteCommandCenter` and `MPNowPlayingInfoCenter`.

## Loading & Error States (not in prototype; please add)
- Loading: badge reads "CONNECTING…" (grey) until `AVPlayerItem.status == .readyToPlay`.
- Error or network drop: badge reads "OFFLINE", programme line reads "Can't reach stream — retrying". Retry with backoff (2s, 5s, 15s).

## State
- `station: "881" | "903"` (saved)
- `isPlaying: Bool`
- `volume: Double` (saved)
- `sleepEnd: Date?`, `sleepMinutes: Int`
- `launchAtLogin: Bool` (read from SMAppService)
- `programme: (name, timeRange)` per station, refreshed every minute

## Design Tokens
- Font: system (SF Pro). Sizes 10 / 11 / 12 / 13 / 15pt.
- Accent: system accent (prototype uses #0A64D8).
- Live red: #E5342B.
- Secondary text: labelColor at 50%.
- Row highlight: 8% black, 6pt radius, 5pt horizontal inset.
- Separators: 0.5pt, 14% black, 14pt inset.
- Menu padding: 6pt vertical; rows 14pt horizontal padding.

## Assets
None. Use SF Symbols (`waveform`, `play.fill`, `pause.fill`, `speaker.fill`, `moon.zzz` optional).

## Files
- `Radio Menu Bar.dc.html`: interactive prototype (open in a browser; needs `support.js` next to it).
