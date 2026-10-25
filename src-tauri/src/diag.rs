//! Source-agnostic diagnostics: a log stream and a network-request log.
//!
//! * Entries are tagged with a free-form `source` string (`app`, `proxy`, `schedule`, `player`,
//!   `tray`, `window`, `provider:<id>`, ...). Nothing here knows about a particular broadcaster,
//!   so future stream providers just log with their own `provider:<id>` tag.
//! * Raw entries live in in-memory ring buffers (1000 log entries, 500 network entries).
//! * Every entry is also written, **always redacted**, to the log file via the `log` crate
//!   (`tauri-plugin-log`, `~/Library/Logs/<bundle id>/radio.log`).
//! * While the Logs window is open, new entries are pushed to it as events, redacted unless
//!   the user enabled "Show sensitive values" (session-only, never saved).

use crate::redact;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

pub const LOGS_WINDOW: &str = "logs";
const MAX_LOGS: usize = 1000;
const MAX_NET: usize = 500;
pub const BODY_PREVIEW_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl From<Level> for log::Level {
    fn from(l: Level) -> Self {
        match l {
            Level::Debug => log::Level::Debug,
            Level::Info => log::Level::Info,
            Level::Warn => log::Level::Warn,
            Level::Error => log::Level::Error,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: u64,
    /// Unix epoch milliseconds
    pub ts: i64,
    pub level: Level,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetEntry {
    pub id: u64,
    pub ts: i64,
    pub source: String,
    /// `api`, `redirect`, `playlist`, `segment`, ...
    pub kind: String,
    /// Station code, when the request belongs to one (used for grouping segments).
    pub station: Option<String>,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub size: Option<u64>,
    pub duration_ms: u64,
    pub req_headers: Vec<(String, String)>,
    pub resp_headers: Vec<(String, String)>,
    /// First ~2 KB of text responses.
    pub body: Option<String>,
    pub error: Option<String>,
}

impl LogEntry {
    fn redacted(&self) -> Self {
        Self { message: redact::text(&self.message), ..self.clone() }
    }
}

impl NetEntry {
    pub fn redacted(&self) -> Self {
        Self {
            url: redact::url(&self.url),
            req_headers: redact::headers(&self.req_headers),
            resp_headers: redact::headers(&self.resp_headers),
            body: self.body.as_deref().map(redact::text),
            error: self.error.as_deref().map(redact::text),
            ..self.clone()
        }
    }
    pub fn failed(&self) -> bool {
        self.error.is_some() || self.status.is_none_or(|s| s >= 400)
    }
}

struct Diag {
    logs: Mutex<VecDeque<LogEntry>>,
    net: Mutex<VecDeque<NetEntry>>,
    next_id: AtomicU64,
    app: OnceLock<AppHandle>,
    window_open: AtomicBool,
    reveal: AtomicBool,
}

fn diag() -> &'static Diag {
    static D: OnceLock<Diag> = OnceLock::new();
    D.get_or_init(|| Diag {
        logs: Mutex::new(VecDeque::with_capacity(MAX_LOGS)),
        net: Mutex::new(VecDeque::with_capacity(MAX_NET)),
        next_id: AtomicU64::new(1),
        app: OnceLock::new(),
        window_open: AtomicBool::new(false),
        reveal: AtomicBool::new(false),
    })
}

pub fn init(app: &AppHandle) {
    let _ = diag().app.set(app.clone());
}

pub fn set_window_open(open: bool) {
    diag().window_open.store(open, Ordering::SeqCst);
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn emit<T: Serialize + Clone>(event: &str, payload: T) {
    let d = diag();
    if !d.window_open.load(Ordering::SeqCst) {
        return;
    }
    if let Some(app) = d.app.get() {
        let _ = app.emit_to(LOGS_WINDOW, event, payload);
    }
}

/// Writes to the log file (through `tauri-plugin-log`, which redacts in its formatter).
fn to_file(level: Level, source: &str, msg: &str) {
    let target = format!("radio::{source}");
    log::logger().log(
        &log::Record::builder()
            .level(level.into())
            .target(&target)
            .args(format_args!("{msg}"))
            .build(),
    );
}

pub fn log(level: Level, source: &str, message: impl Into<String>) {
    let d = diag();
    let entry = LogEntry {
        id: d.next_id.fetch_add(1, Ordering::Relaxed),
        ts: now_ms(),
        level,
        source: source.to_string(),
        message: message.into(),
    };
    to_file(level, source, &entry.message);
    {
        let mut logs = d.logs.lock().unwrap();
        if logs.len() >= MAX_LOGS {
            logs.pop_front();
        }
        logs.push_back(entry.clone());
    }
    let reveal = d.reveal.load(Ordering::SeqCst);
    emit("diag-log", if reveal { entry } else { entry.redacted() });
}

pub fn debug(source: &str, m: impl Into<String>) {
    log(Level::Debug, source, m)
}
pub fn info(source: &str, m: impl Into<String>) {
    log(Level::Info, source, m)
}
pub fn warn(source: &str, m: impl Into<String>) {
    log(Level::Warn, source, m)
}
pub fn error(source: &str, m: impl Into<String>) {
    log(Level::Error, source, m)
}

pub fn record_net(mut entry: NetEntry) {
    let d = diag();
    entry.id = d.next_id.fetch_add(1, Ordering::Relaxed);
    if entry.ts == 0 {
        entry.ts = now_ms();
    }
    let status = entry.status.map(|s| s.to_string()).unwrap_or_else(|| "ERR".into());
    let level = if entry.failed() { Level::Warn } else { Level::Debug };
    to_file(
        level,
        &format!("net:{}", entry.source),
        &format!(
            "{} {} -> {status} {} {}B {}ms{}",
            entry.method,
            entry.url,
            entry.content_type.as_deref().unwrap_or("-"),
            entry.size.map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
            entry.duration_ms,
            entry.error.as_deref().map(|e| format!(" error: {e}")).unwrap_or_default()
        ),
    );
    {
        let mut net = d.net.lock().unwrap();
        if net.len() >= MAX_NET {
            net.pop_front();
        }
        net.push_back(entry.clone());
    }
    let reveal = d.reveal.load(Ordering::SeqCst);
    emit("diag-net", if reveal { entry } else { entry.redacted() });
}

// ---------------------------------------------------------------- log file

pub fn log_file_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_log_dir().ok().map(|d| d.join("radio.log"))
}

/// `tauri-plugin-log` configured so the file is always redacted.
pub fn log_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_log::{RotationStrategy, Target, TargetKind};
    tauri_plugin_log::Builder::new()
        .clear_targets()
        .targets([
            Target::new(TargetKind::LogDir { file_name: Some("radio".into()) }),
            Target::new(TargetKind::Stderr),
        ])
        .level(log::LevelFilter::Debug)
        // our own records use `radio::<source>` targets; other crates only above Info
        .filter(|m| m.target().starts_with("radio::") || m.level() <= log::Level::Warn)
        .max_file_size(2_000_000)
        .rotation_strategy(RotationStrategy::KeepOne)
        .format(|out, message, record| {
            let target = record.target().strip_prefix("radio::").unwrap_or(record.target());
            out.finish(format_args!(
                "{} {:<5} [{}] {}",
                iso_utc(now_ms()),
                record.level(),
                target,
                redact::text(&message.to_string())
            ))
        })
        .build()
}

/// `2026-10-25T03:12:45.123Z` without pulling in a date crate.
fn iso_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60,
        ms.rem_euclid(1000)
    )
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    logs: Vec<LogEntry>,
    net: Vec<NetEntry>,
    reveal: bool,
    log_file: Option<String>,
}

fn snapshot(app: &AppHandle) -> Snapshot {
    let d = diag();
    let reveal = d.reveal.load(Ordering::SeqCst);
    let logs = d.logs.lock().unwrap().iter().map(|e| if reveal { e.clone() } else { e.redacted() }).collect();
    let net = d.net.lock().unwrap().iter().map(|e| if reveal { e.clone() } else { e.redacted() }).collect();
    Snapshot { logs, net, reveal, log_file: log_file_path(app).map(|p| p.display().to_string()) }
}

#[tauri::command]
pub fn diag_snapshot(app: AppHandle) -> Snapshot {
    snapshot(&app)
}

/// Session-only: not saved, so it resets to off on every launch.
#[tauri::command]
pub fn diag_set_reveal(app: AppHandle, reveal: bool) -> Snapshot {
    diag().reveal.store(reveal, Ordering::SeqCst);
    snapshot(&app)
}

#[tauri::command]
pub fn diag_clear(which: String) {
    let d = diag();
    match which.as_str() {
        "logs" => d.logs.lock().unwrap().clear(),
        "net" => d.net.lock().unwrap().clear(),
        _ => {
            d.logs.lock().unwrap().clear();
            d.net.lock().unwrap().clear();
        }
    }
}

/// Lets the webview (player events etc.) log into the same stream.
#[tauri::command]
pub fn log_from_ui(level: Level, source: String, message: String) {
    let source: String = source.chars().filter(|c| c.is_ascii_alphanumeric() || ":-_.".contains(*c)).take(40).collect();
    let source = if source.is_empty() { "ui".to_string() } else { source };
    log(level, &source, message);
}

/// Saves the visible rows (already redacted unless "Show sensitive values" is on) through a
/// native save dialog. The format follows the chosen extension (.json or .txt).
/// `RADIO_DEBUG_EXPORT_TO=/path/file.txt` skips the dialog (for automated checks).
#[tauri::command]
pub async fn export_diag(app: AppHandle, txt: String, json: String, file_name: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let path = match std::env::var("RADIO_DEBUG_EXPORT_TO") {
        Ok(p) if !p.is_empty() => Some(std::path::PathBuf::from(p)),
        _ => app
            .dialog()
            .file()
            .set_title("Export")
            .set_file_name(&file_name)
            .add_filter("Text", &["txt"])
            .add_filter("JSON", &["json"])
            .blocking_save_file()
            .and_then(|p| p.into_path().ok()),
    };
    let Some(path) = path else { return Ok(None) };
    let is_json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    std::fs::write(&path, if is_json { json } else { txt }).map_err(|e| e.to_string())?;
    info("app", format!("exported diagnostics to {}", path.display()));
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
pub fn reveal_log_file(app: AppHandle) -> Result<(), String> {
    let path = log_file_path(&app).ok_or("no log directory")?;
    let target = if path.exists() { path } else { path.parent().map(|p| p.to_path_buf()).ok_or("no log directory")? };
    std::process::Command::new("open").arg("-R").arg(&target).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso_dates() {
        assert_eq!(super::iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::iso_utc(1_792_893_116_123), "2026-10-25T01:51:56.123Z");
    }
}
