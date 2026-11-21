mod config;
mod diag;
mod net;
#[cfg(target_os = "macos")]
mod popover_monitors;
#[cfg(target_os = "macos")]
mod power;
mod proxy;
mod redact;
mod resolver;
mod schedule;
mod tray;

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

const POPOVER: &str = "main";
const POPOVER_WIDTH: f64 = 300.0;
/// Gap between the menu bar and the popover, like a native status menu.
const POPOVER_GAP: f64 = 4.0;

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// Debug switches (env vars), used for screenshots and testing:
///   RADIO_DEBUG_SHOW=1      open the popover at launch
///   RADIO_DEBUG_PIN=1       keep the popover open on blur / outside clicks (for screenshots)
///   RADIO_DEBUG_AUTOPLAY=1  start playback at launch
///   RADIO_DEBUG_LOGS=1      open the Logs window at launch
///   RADIO_DEBUG_EXPORT_TO=/path/file.txt  Export writes there instead of showing a save dialog
///   RADIO_DEBUG_FAIL_STREAM=1  make stream resolution fail (to test OFFLINE + retry backoff)
///   RADIO_DEBUG_THEME=light|dark  force the popover's appearance (default: follow the system)
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct DebugFlags {
    show: bool,
    pin: bool,
    autoplay: bool,
}

struct PopoverState {
    last_hidden: Mutex<Instant>,
    last_toggle: Mutex<Instant>,
    tray_rect: Mutex<Option<tauri::Rect>>,
    debug: DebugFlags,
}

// ---------------------------------------------------------------- popover window

fn popover(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(POPOVER)
}

pub(crate) fn hide_popover_window(app: &AppHandle, reason: &str) {
    #[cfg(target_os = "macos")]
    popover_monitors::remove(app);
    if let Some(w) = popover(app)
        && w.is_visible().unwrap_or(false)
    {
        diag::debug("window", format!("hide popover ({reason})"));
        let _ = w.hide();
        *app.state::<PopoverState>().last_hidden.lock().unwrap() = Instant::now();
    }
}

/// Centres the popover under the status item, 4 pt below the menu bar, kept on the tray's monitor.
///
/// (tauri-plugin-positioner's `TrayCenter` uses the tray rect's y, which on macOS is the *top* of
/// the menu bar, so the window would sit under the menu bar. Window setters are also queued, so
/// correcting its result by reading the position back is racy. The maths is simple, so do it here.)
fn position_popover(app: &AppHandle, w: &WebviewWindow) {
    let ps = app.state::<PopoverState>();
    let mut rect = *ps.tray_rect.lock().unwrap();
    if rect.is_none() {
        // e.g. opened via RADIO_DEBUG_SHOW before any tray event carried the rect
        rect = app.tray_by_id(tray::TRAY_ID).and_then(|t| t.rect().ok().flatten());
    }
    let (Some(r), Ok(size), Ok(scale)) = (rect, w.outer_size(), w.scale_factor()) else {
        diag::warn("window", "popover: tray position unknown");
        return;
    };
    let tray_pos = r.position.to_physical::<f64>(1.0);
    let tray_size = r.size.to_physical::<f64>(1.0);
    let mut x = tray_pos.x + tray_size.width / 2.0 - size.width as f64 / 2.0;
    let y = tray_pos.y + tray_size.height + POPOVER_GAP * scale;
    if let Ok(Some(m)) = w.monitor_from_point(tray_pos.x + tray_size.width / 2.0, tray_pos.y + 1.0) {
        let (mx, mw) = (m.position().x as f64, m.size().width as f64);
        let margin = 6.0 * scale;
        x = x.min(mx + mw - size.width as f64 - margin).max(mx + margin);
    }
    let _ = w.set_position(PhysicalPosition::new(x.round(), y.round()));
}

fn show_popover(app: &AppHandle) {
    let Some(w) = popover(app) else { return };
    position_popover(app, &w);
    diag::debug("window", "show popover");
    let _ = w.show();
    // Makes the window key and activates the app, so blur (Focused(false)) fires reliably.
    let _ = w.set_focus();
    // ...and make the web view first responder, so keys (Escape, Cmd-Q) reach the page.
    let _ = AsRef::<tauri::Webview>::as_ref(&w).set_focus();
    #[cfg(target_os = "macos")]
    if !app.state::<PopoverState>().debug.pin {
        popover_monitors::install(app);
    }
    let _ = app.emit("popover-shown", ());
}

fn toggle_popover(app: &AppHandle) {
    let Some(w) = popover(app) else { return };
    // One physical click can arrive as two Click/Up events; treat them as one.
    {
        let ps = app.state::<PopoverState>();
        let mut last = ps.last_toggle.lock().unwrap();
        if last.elapsed() < Duration::from_millis(350) {
            diag::debug("tray", "tray click ignored: duplicate event");
            return;
        }
        *last = Instant::now();
    }
    if w.is_visible().unwrap_or(false) {
        hide_popover_window(app, "tray click");
        return;
    }
    // A click on the status item while the popover is open may first blur (and hide) the
    // popover; don't immediately re-open it for that same click.
    let recently_hidden = app.state::<PopoverState>().last_hidden.lock().unwrap().elapsed() < Duration::from_millis(250);
    if recently_hidden {
        diag::debug("tray", "tray click ignored: popover was just hidden by the same click");
    } else {
        show_popover(app);
    }
}

/// True if the pointer is over the status item. tray-icon reports `Click` from its view's
/// `mouseUp:`, and AppKit can route a stray mouse-up there (e.g. after a click inside the
/// popover), so a click only counts when the pointer is actually on the icon.
fn pointer_on_tray(rect: &tauri::Rect, pos: &PhysicalPosition<f64>) -> bool {
    let p = rect.position.to_physical::<f64>(1.0);
    let s = rect.size.to_physical::<f64>(1.0);
    let slop = 4.0;
    pos.x >= p.x - slop && pos.x <= p.x + s.width + slop && pos.y >= p.y - slop && pos.y <= p.y + s.height + slop
}

/// ⌥ held during the tray click: read from the click event itself (NSApp.currentEvent),
/// falling back to the global modifier state.
#[cfg(target_os = "macos")]
fn option_key_down() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags};
    let opt = NSEventModifierFlags::Option.0;
    let from_event = MainThreadMarker::new()
        .and_then(|mtm| NSApplication::sharedApplication(mtm).currentEvent())
        .is_some_and(|e| e.modifierFlags().0 & opt != 0);
    from_event || NSEvent::modifierFlags_class().0 & opt != 0
}
#[cfg(not(target_os = "macos"))]
fn option_key_down() -> bool {
    false
}

pub(crate) fn on_tray_event(app: &AppHandle, event: TrayIconEvent) {
    match event {
        TrayIconEvent::Click { rect, position, button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            *app.state::<PopoverState>().tray_rect.lock().unwrap() = Some(rect);
            if !pointer_on_tray(&rect, &position) {
                diag::debug("tray", "ignored stray tray click (pointer not on the status item)");
            } else if option_key_down() {
                diag::debug("tray", "option-click: open logs");
                open_logs_window(app);
            } else {
                toggle_popover(app);
            }
        }
        TrayIconEvent::Click { rect, .. } | TrayIconEvent::Enter { rect, .. } | TrayIconEvent::Move { rect, .. } => {
            *app.state::<PopoverState>().tray_rect.lock().unwrap() = Some(rect);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- logs window

/// Opens (or brings forward) the "Radio — Logs" window. While it exists the app is a regular
/// app with a Dock icon and a ⌘-Tab entry; it goes back to menu-bar-only when it closes.
pub(crate) fn open_logs_window(app: &AppHandle) {
    hide_popover_window(app, "opening logs");
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window(diag::LOGS_WINDOW) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        diag::debug("window", "logs window brought to front");
        return;
    }
    diag::set_window_open(true);
    match WebviewWindowBuilder::new(app, diag::LOGS_WINDOW, WebviewUrl::App("logs.html".into()))
        .title("Radio — Logs")
        .inner_size(900.0, 560.0)
        .min_inner_size(640.0, 360.0)
        .resizable(true)
        .center()
        .focused(true)
        .build()
    {
        Ok(w) => {
            let _ = w.set_focus();
            diag::debug("window", "logs window opened");
        }
        Err(e) => {
            diag::set_window_open(false);
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            diag::error("window", format!("could not open logs window: {e}"));
        }
    }
}

fn on_logs_window_closed(app: &AppHandle) {
    diag::set_window_open(false);
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    diag::debug("window", "logs window closed; back to menu-bar-only");
}

// ---------------------------------------------------------------- commands

#[tauri::command]
async fn prepare_stream(proxy: tauri::State<'_, Arc<proxy::StreamProxy>>, code: String) -> Result<String, String> {
    let proxy = proxy.inner().clone();
    let c = code.clone();
    let result = tauri::async_runtime::spawn_blocking(move || proxy.prepare(&c)).await.map_err(|e| e.to_string())?;
    if let Err(e) = &result {
        diag::warn("proxy", format!("prepare_stream({code}) failed: {e}"));
    }
    result
}

#[tauri::command]
async fn fetch_schedule() -> Result<HashMap<String, Vec<schedule::Programme>>, String> {
    let result = tauri::async_runtime::spawn_blocking(schedule::fetch).await.map_err(|e| e.to_string())?;
    if let Err(e) = &result {
        diag::warn("schedule", format!("schedule unavailable: {e}"));
    }
    result
}

#[tauri::command]
fn set_playback_state(app: AppHandle, playing: bool, code: String) {
    {
        let ctl = app.state::<tray::TrayCtl>();
        let mut st = ctl.state.lock().unwrap();
        st.playing = playing;
        st.code = code;
    }
    tray::refresh_title(&app);
}

#[tauri::command]
fn set_label_mode(app: AppHandle, show_label: bool) {
    app.state::<tray::TrayCtl>().state.lock().unwrap().show_label = show_label;
    tray::sync_label_item(&app, show_label);
    tray::refresh_title(&app);
}

/// 0 cancels. Returns the seconds remaining (None when off).
#[tauri::command]
fn set_sleep_timer(app: AppHandle, minutes: u32) -> Option<u64> {
    let ctl = app.state::<tray::TrayCtl>();
    ctl.state.lock().unwrap().sleep_end = (minutes > 0).then(|| Instant::now() + Duration::from_secs(minutes as u64 * 60));
    diag::info("app", if minutes > 0 { format!("sleep timer set: {minutes} min") } else { "sleep timer off".into() });
    tray::refresh_title(&app);
    ctl.sleep_remaining()
}

#[tauri::command]
fn hide_popover(app: AppHandle) {
    hide_popover_window(&app, "escape");
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn open_logs(app: AppHandle) {
    open_logs_window(&app);
}

/// The UI measures its content and asks for the window to fit it.
#[tauri::command]
fn resize_popover(window: WebviewWindow, height: f64) {
    let h = height.clamp(100.0, 900.0).ceil();
    if let Ok(cur) = window.inner_size() {
        let scale = window.scale_factor().unwrap_or(1.0);
        if (cur.height as f64 / scale - h).abs() < 0.5 {
            return;
        }
    }
    let _ = window.set_size(LogicalSize::new(POPOVER_WIDTH, h));
}

#[tauri::command]
fn debug_flags(state: tauri::State<'_, PopoverState>) -> DebugFlags {
    state.debug
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let debug = DebugFlags {
        show: env_flag("RADIO_DEBUG_SHOW"),
        pin: env_flag("RADIO_DEBUG_PIN"),
        autoplay: env_flag("RADIO_DEBUG_AUTOPLAY"),
    };
    let open_logs_at_launch = env_flag("RADIO_DEBUG_LOGS");

    tauri::Builder::default()
        .plugin(diag::log_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .manage(tray::TrayCtl::new())
        .manage(PopoverState {
            last_hidden: Mutex::new(Instant::now() - Duration::from_secs(10)),
            last_toggle: Mutex::new(Instant::now() - Duration::from_secs(10)),
            tray_rect: Mutex::new(None),
            debug,
        })
        .invoke_handler(tauri::generate_handler![
            prepare_stream,
            fetch_schedule,
            set_playback_state,
            set_label_mode,
            set_sleep_timer,
            hide_popover,
            quit_app,
            open_logs,
            resize_popover,
            debug_flags,
            diag::diag_snapshot,
            diag::diag_set_reveal,
            diag::diag_clear,
            diag::log_from_ui,
            diag::export_diag,
            diag::reveal_log_file
        ])
        .setup(move |app| {
            diag::init(app.handle());
            diag::info("app", format!("Radio {} starting", app.package_info().version));

            // Menu bar only: no Dock icon (until the Logs window is opened).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let proxy = proxy::StreamProxy::start().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            app.manage(proxy);

            tray::build(app.handle())?;

            #[cfg(target_os = "macos")]
            power::install(app.handle());

            if let Some(w) = popover(app.handle()) {
                #[cfg(target_os = "macos")]
                {
                    use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState, apply_vibrancy};
                    if let Err(e) = apply_vibrancy(&w, NSVisualEffectMaterial::Menu, Some(NSVisualEffectState::Active), Some(12.0)) {
                        diag::warn("window", format!("vibrancy: {e}"));
                    }
                }
                let _ = w.set_size(LogicalSize::new(POPOVER_WIDTH, 460.0));
                match std::env::var("RADIO_DEBUG_THEME").as_deref() {
                    Ok("light") => {
                        let _ = w.set_theme(Some(tauri::Theme::Light));
                    }
                    Ok("dark") => {
                        let _ = w.set_theme(Some(tauri::Theme::Dark));
                    }
                    _ => {}
                }
            }

            if debug.show || open_logs_at_launch {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1200));
                    let h = handle.clone();
                    let _ = handle.run_on_main_thread(move || {
                        if open_logs_at_launch {
                            open_logs_window(&h);
                        }
                        if debug.show {
                            show_popover(&h);
                        }
                    });
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| match window.label() {
            POPOVER => match event {
                WindowEvent::Focused(false) => {
                    let app = window.app_handle();
                    if !app.state::<PopoverState>().debug.pin {
                        hide_popover_window(app, "blur");
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    hide_popover_window(window.app_handle(), "close");
                }
                _ => {}
            },
            diag::LOGS_WINDOW => {
                if let WindowEvent::Destroyed = event {
                    on_logs_window_closed(window.app_handle());
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
