//! Menu bar status item: animated 3-bar template icon, "903 29:41" title, right-click menu,
//! and the sleep-timer clock (kept in Rust so it keeps exact time while the popover's webview
//! is hidden and its JS timers may be throttled).

use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, Wry};

pub const TRAY_ID: &str = "radio";
const FRAME_MS: u64 = 120;

macro_rules! frames {
    ($($n:literal),*) => { [$(include_bytes!(concat!("../icons/tray/bars-", $n, ".png")).as_slice()),*] };
}
const FRAME_PNGS: [&[u8]; 24] = frames!(
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
    "17", "18", "19", "20", "21", "22", "23"
);
const PAUSED_PNG: &[u8] = include_bytes!("../icons/tray/bars-paused.png");

#[derive(Debug)]
pub struct TrayState {
    /// Audio is actually playing (LIVE) -> animate the bars.
    pub playing: bool,
    pub code: String,
    pub show_label: bool,
    pub sleep_end: Option<Instant>,
    last_title: Option<String>,
}

pub struct TrayCtl {
    pub state: Mutex<TrayState>,
    pub label_item: Mutex<Option<CheckMenuItem<Wry>>>,
}

impl TrayCtl {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TrayState {
                playing: false,
                code: "903".into(),
                show_label: true,
                sleep_end: None,
                last_title: None,
            }),
            label_item: Mutex::new(None),
        }
    }

    pub fn sleep_remaining(&self) -> Option<u64> {
        let st = self.state.lock().unwrap();
        st.sleep_end.map(|end| end.saturating_duration_since(Instant::now()).as_secs_f64().round() as u64)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Tick {
    /// Seconds left on the sleep timer, if one is running.
    sleep_remaining: Option<u64>,
}

fn fmt_mmss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Recomputes and applies the status item title if it changed.
pub fn refresh_title(app: &AppHandle) {
    let ctl = app.state::<TrayCtl>();
    let remaining = ctl.sleep_remaining();
    let mut st = ctl.state.lock().unwrap();
    let mut parts = Vec::new();
    if st.show_label {
        parts.push(st.code.clone());
    }
    if let Some(r) = remaining {
        parts.push(fmt_mmss(r));
    }
    let title = if parts.is_empty() { None } else { Some(parts.join(" ")) };
    if st.last_title != title {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_title(title.as_deref());
        }
        st.last_title = title;
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let show_label = CheckMenuItem::with_id(app, "show-label", "Show Station in Menu Bar", true, true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "open-logs", "Open Logs…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Radio", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(
        app,
        &[&show_label, &logs, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    *app.state::<TrayCtl>().label_item.lock().unwrap() = Some(show_label);

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(PAUSED_PNG)?)
        .icon_as_template(true)
        .title("903")
        .tooltip("Radio")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "quit" => app.exit(0),
            "open-logs" => crate::open_logs_window(app),
            "show-label" => {
                let ctl = app.state::<TrayCtl>();
                let show = {
                    let mut st = ctl.state.lock().unwrap();
                    st.show_label = !st.show_label;
                    st.show_label
                };
                sync_label_item(app, show);
                refresh_title(app);
                // Let the UI persist the setting and update its switch.
                crate::diag::debug("tray", format!("menu: show station in menu bar = {show}"));
                let _ = app.emit("label-mode-changed", show);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| crate::on_tray_event(tray.app_handle(), event))
        .build(app)?;

    spawn_animator(app.clone())?;
    spawn_ticker(app.clone())?;
    Ok(tray)
}

pub fn sync_label_item(app: &AppHandle, show: bool) {
    if let Some(item) = app.state::<TrayCtl>().label_item.lock().unwrap().as_ref() {
        let _ = item.set_checked(show);
    }
}

/// Cycles the template frames while playing; one static frame (bars at 35%) when not.
fn spawn_animator(app: AppHandle) -> tauri::Result<()> {
    let frames: Vec<Image<'static>> = FRAME_PNGS
        .iter()
        .map(|b| Image::from_bytes(b).map(|i| i.to_owned()))
        .collect::<tauri::Result<_>>()?;
    let paused = Image::from_bytes(PAUSED_PNG)?.to_owned();
    std::thread::Builder::new()
        .name("tray-animator".into())
        .spawn(move || {
            let mut frame = 0usize;
            let mut showing_paused = true;
            loop {
                std::thread::sleep(Duration::from_millis(FRAME_MS));
                let playing = app.state::<TrayCtl>().state.lock().unwrap().playing;
                let Some(tray) = app.tray_by_id(TRAY_ID) else { continue };
                if playing {
                    frame = (frame + 1) % frames.len();
                    let _ = tray.set_icon_with_as_template(Some(frames[frame].clone()), true);
                    showing_paused = false;
                } else if !showing_paused {
                    let _ = tray.set_icon_with_as_template(Some(paused.clone()), true);
                    showing_paused = true;
                }
            }
        })?;
    Ok(())
}

/// 1 s clock: updates the title countdown, fires the sleep timer, and sends a `tick` to the UI.
fn spawn_ticker(app: AppHandle) -> tauri::Result<()> {
    std::thread::Builder::new()
        .name("tray-ticker".into())
        .spawn(move || loop {
            // align roughly to wall-clock seconds so the countdown ticks evenly
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            std::thread::sleep(Duration::from_millis(1000 - (now.subsec_millis() as u64) + 5));

            let ctl = app.state::<TrayCtl>();
            let expired = {
                let mut st = ctl.state.lock().unwrap();
                match st.sleep_end {
                    Some(end) if Instant::now() >= end => {
                        st.sleep_end = None;
                        st.playing = false;
                        true
                    }
                    _ => false,
                }
            };
            if expired {
                crate::diag::info("app", "sleep timer expired: pausing");
                let _ = app.emit("sleep-expired", ());
            }
            refresh_title(&app);
            let _ = app.emit("tick", Tick { sleep_remaining: ctl.sleep_remaining() });
        })?;
    Ok(())
}
